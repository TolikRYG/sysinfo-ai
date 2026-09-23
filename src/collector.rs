//! Local-only collection. This module never opens a network connection or saves a file.
//! WMI connections are created and used inside the same hardware worker thread.

use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System};

const HARDWARE_TIMEOUT: Duration = Duration::from_secs(20);
static HARDWARE_BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Default, Serialize)]
pub struct SystemReport {
    pub schema_version: u32,
    pub collected_at_unix_seconds: u64,
    pub collection_duration_ms: u64,
    pub cpu_sample_interval_ms: u64,
    pub privacy: PrivacyInfo,
    pub os: OsInfo,
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    /// Mounted volumes, not necessarily separate physical drives.
    pub disks: Vec<DiskInfo>,
    pub physical_disks: Vec<PhysicalDiskInfo>,
    /// Driver identity and legacy memory values from Win32_VideoController.
    pub gpu: Vec<GpuInfo>,
    /// Separate adapter list; never join WMI and DXGI by enumeration order.
    pub dxgi_adapters: Vec<DxgiAdapterInfo>,
    pub motherboards: Vec<MotherboardInfo>,
    pub bios: Vec<BiosInfo>,
    pub network: Vec<NetworkInfo>,
    pub top_processes: Vec<ProcessInfo>,
    pub warnings: Vec<CollectionWarning>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PrivacyInfo {
    pub identifiers_redacted: bool,
    pub process_list_omitted: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PrivacyOptions {
    pub hide_identifiers: bool,
    pub include_processes: bool,
}

impl Default for PrivacyOptions {
    fn default() -> Self {
        Self {
            hide_identifiers: true,
            include_processes: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Size {
    pub bytes: u64,
    /// Binary units: 1 GiB = 1,073,741,824 bytes, not decimal GB.
    pub gib: f64,
}

impl From<u64> for Size {
    fn from(bytes: u64) -> Self {
        Self {
            bytes,
            gib: bytes as f64 / 1_073_741_824.0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OsInfo {
    pub name: Option<String>,
    pub version: Option<String>,
    pub long_version: Option<String>,
    pub kernel_version: Option<String>,
    pub hostname: Option<String>,
    pub architecture: String,
    pub uptime_seconds: u64,
    pub boot_time_unix_seconds: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuInfo {
    pub brand: Option<String>,
    pub physical_cores: Option<usize>,
    pub logical_cores: usize,
    pub reported_frequency_mhz: Option<u64>,
    pub usage_percent: Option<f32>,
    pub logical_processors: Vec<LogicalCpuInfo>,
    pub packages_wmi: Vec<CpuPackageInfo>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LogicalCpuInfo {
    pub name: String,
    pub brand: String,
    pub vendor_id: String,
    pub reported_frequency_mhz: Option<u64>,
    pub usage_percent: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuPackageInfo {
    pub name: Option<String>,
    pub manufacturer: Option<String>,
    pub socket: Option<String>,
    pub physical_cores: Option<u64>,
    pub logical_cores: Option<u64>,
    pub current_clock_mhz_reported: Option<u64>,
    pub max_clock_mhz_reported: Option<u64>,
    pub l2_cache_kib: Option<u64>,
    pub l3_cache_kib: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryInfo {
    pub total: Size,
    pub used: Size,
    pub available: Size,
    pub free: Size,
    pub usage_percent: Option<f64>,
    pub swap_total: Size,
    pub swap_used: Size,
    pub swap_free: Size,
    pub modules: Vec<MemoryModule>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryModule {
    pub slot: Option<String>,
    pub bank: Option<String>,
    pub manufacturer: Option<String>,
    pub part_number: Option<String>,
    pub serial_number: Option<String>,
    pub capacity: Option<Size>,
    pub speed_mhz_reported: Option<u64>,
    pub configured_clock_speed_mhz_reported: Option<u64>,
    pub smbios_memory_type_code: Option<u64>,
    pub form_factor_code: Option<u64>,
    pub data_width_bits: Option<u64>,
    pub total_width_bits: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub total: Size,
    pub free: Size,
    pub used: Size,
    pub file_system: String,
    pub kind_reported: String,
    pub is_removable: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PhysicalDiskInfo {
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub serial_number: Option<String>,
    pub firmware_revision: Option<String>,
    pub interface_type_reported: Option<String>,
    pub media_type_reported: Option<String>,
    pub size: Option<Size>,
    pub status_reported: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GpuInfo {
    pub name: Option<String>,
    pub manufacturer: Option<String>,
    pub video_processor: Option<String>,
    pub driver_version: Option<String>,
    pub driver_date_wmi: Option<String>,
    /// WMI AdapterRAM is uint32. NOT a verified dedicated VRAM size.
    pub adapter_ram_wmi_reported: Option<Size>,
    pub adapter_ram_is_reliable: bool,
    pub current_horizontal_resolution: Option<u64>,
    pub current_vertical_resolution: Option<u64>,
    pub current_refresh_rate_hz_reported: Option<u64>,
    pub status_reported: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DxgiAdapterInfo {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_video_memory: Size,
    pub dedicated_system_memory: Size,
    /// Shared system RAM limit, NOT additional dedicated VRAM.
    pub shared_system_memory: Size,
    pub is_software_adapter: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MotherboardInfo {
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub version: Option<String>,
    pub serial_number: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BiosInfo {
    pub manufacturer: Option<String>,
    pub smbios_bios_version: Option<String>,
    pub version_strings: Vec<String>,
    pub serial_number: Option<String>,
    /// Original DMTF timestamp, including its UTC offset when present.
    pub release_date_wmi: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetworkInfo {
    pub interface: String,
    pub mac_address: Option<String>,
    pub total_received_bytes: u64,
    pub total_transmitted_bytes: u64,
    pub total_received_packets: u64,
    pub total_transmitted_packets: u64,
    pub total_receive_errors: u64,
    pub total_transmit_errors: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub memory_bytes: u64,
    pub memory_mib: f64,
    /// Normalized to the capacity of the entire machine (0..100), not one core.
    pub cpu_percent_of_system: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CollectionWarning {
    pub component: String,
    pub message: String,
}

fn warn(report: &mut SystemReport, component: &str, message: impl Into<String>) {
    report.warnings.push(CollectionWarning {
        component: component.into(),
        message: message.into(),
    });
}

fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn finite_percent(value: f32) -> Option<f32> {
    value.is_finite().then(|| value.clamp(0.0, 100.0))
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |v| v.as_secs())
}

/// Must be called from a background thread, never from egui::App::update.
pub fn collect_all() -> Result<SystemReport> {
    let start = Instant::now();
    let mut sys = System::new();
    let process_refresh = || ProcessRefreshKind::nothing().with_memory().with_cpu();
    // Avoid System::new_all(): command lines, environment, executable paths and
    // user information are not needed for the requested inventory.
    sys.refresh_cpu_all();
    sys.refresh_memory();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh());
    let sample_start = Instant::now();
    thread::sleep(Duration::from_millis(750).max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL));
    sys.refresh_cpu_usage();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh());
    sys.refresh_memory();
    let sample_ms = sample_start.elapsed().as_millis() as u64;
    let logical_count = sys.cpus().len();

    let mut report = SystemReport {
        schema_version: 2,
        collected_at_unix_seconds: unix_now(),
        cpu_sample_interval_ms: sample_ms,
        os: OsInfo {
            name: System::name(),
            version: System::os_version(),
            long_version: System::long_os_version(),
            kernel_version: System::kernel_version(),
            hostname: System::host_name(),
            architecture: System::cpu_arch(),
            uptime_seconds: System::uptime(),
            boot_time_unix_seconds: System::boot_time(),
        },
        cpu: CpuInfo {
            brand: sys.cpus().first().map(|v| v.brand().trim().to_owned()),
            physical_cores: sys.physical_core_count(),
            logical_cores: logical_count,
            reported_frequency_mhz: sys.cpus().first().and_then(|v| nonzero(v.frequency())),
            usage_percent: (logical_count > 0).then(|| finite_percent(sys.global_cpu_usage())).flatten(),
            logical_processors: sys.cpus().iter().map(|v| LogicalCpuInfo {
                name: v.name().to_owned(),
                brand: v.brand().trim().to_owned(),
                vendor_id: v.vendor_id().to_owned(),
                reported_frequency_mhz: nonzero(v.frequency()),
                usage_percent: finite_percent(v.cpu_usage()),
            }).collect(),
            ..Default::default()
        },
        memory: MemoryInfo {
            total: sys.total_memory().into(),
            used: sys.used_memory().into(),
            available: sys.available_memory().into(),
            free: sys.free_memory().into(),
            usage_percent: (sys.total_memory() > 0)
                .then(|| 100.0 * sys.used_memory() as f64 / sys.total_memory() as f64),
            swap_total: sys.total_swap().into(),
            swap_used: sys.used_swap().into(),
            swap_free: sys.free_swap().into(),
            modules: Vec::new(),
        },
        limitations: [
            "Это разовый снимок, а не бенчмарк. По текущей загрузке нельзя доказать постоянное узкое место.",
            "GiB = 2^30 байт; MiB = 2^20 байт. Частоты CPU сообщены ОС/драйвером и не гарантируют фактический turbo-clock.",
            "WMI AdapterRAM — uint32: объём VRAM может быть обрезан/неверен, особенно от 4 GiB. Предпочитайте отдельный список dxgi_adapters. Не сопоставляйте адаптеры по порядку; одинаковые названия могут быть неоднозначны.",
            "DXGI сообщает данные драйвера. SharedSystemMemory — доступная общая RAM, не выделенная VRAM. Для встроенной GPU небольшой DedicatedVideoMemory не равен всей доступной памяти.",
            "WMI Speed и ConfiguredClockSpeed сохранены как сообщённые значения. Не делайте автоматических выводов о MT/s, реальной тактовой частоте, XMP/EXPO или числе каналов.",
            "disks — смонтированные тома, physical_disks — отдельная WMI-инвентаризация устройств. Связь между ними не установлена; не складывайте их ёмкости.",
            "Сетевые значения — накопленные счётчики ОС за период жизни/сброса интерфейса, не скорость соединения и не обязательно трафик с загрузки Windows.",
            "Swap — показатели sysinfo/Windows, не доказательство активного свопинга. Память процессов — резидентная память, её сумма не равна всей использованной RAM.",
            "Температуры, SMART, состояние БП, батареи, лицензии, загрузка GPU и скорости накопителей здесь не измеряются. Список процессов может быть неполным из-за прав доступа.",
            "null означает, что значение не получено; пустой список может означать отсутствие устройств или недоступность источника — смотрите warnings. Части снимка собраны в немного разное время.",
        ].into_iter().map(str::to_owned).collect(),
        ..Default::default()
    };

    let disks = Disks::new_with_refreshed_list();
    report.disks = disks
        .iter()
        .map(|d| DiskInfo {
            name: d.name().to_string_lossy().into_owned(),
            mount_point: d.mount_point().to_string_lossy().into_owned(),
            total: d.total_space().into(),
            free: d.available_space().into(),
            used: d.total_space().saturating_sub(d.available_space()).into(),
            file_system: d.file_system().to_string_lossy().into_owned(),
            kind_reported: format!("{:?}", d.kind()),
            is_removable: d.is_removable(),
        })
        .collect();
    report
        .disks
        .sort_by(|a, b| a.mount_point.cmp(&b.mount_point));

    let networks = Networks::new_with_refreshed_list();
    report.network = networks
        .iter()
        .map(|(name, n)| {
            let mac = n.mac_address().to_string();
            NetworkInfo {
                interface: name.clone(),
                mac_address: (mac != "00:00:00:00:00:00").then_some(mac),
                total_received_bytes: n.total_received(),
                total_transmitted_bytes: n.total_transmitted(),
                total_received_packets: n.total_packets_received(),
                total_transmitted_packets: n.total_packets_transmitted(),
                total_receive_errors: n.total_errors_on_received(),
                total_transmit_errors: n.total_errors_on_transmitted(),
            }
        })
        .collect();
    report.network.sort_by(|a, b| a.interface.cmp(&b.interface));

    report.top_processes = sys
        .processes()
        .iter()
        .map(|(pid, p)| ProcessInfo {
            pid: pid.as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            memory_bytes: p.memory(),
            memory_mib: p.memory() as f64 / 1_048_576.0,
            cpu_percent_of_system: (logical_count > 0)
                .then(|| finite_percent(p.cpu_usage() / logical_count as f32))
                .flatten(),
        })
        .collect();
    report
        .top_processes
        .sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes).then(a.pid.cmp(&b.pid)));
    report.top_processes.truncate(10);

    if logical_count == 0 {
        warn(
            &mut report,
            "sysinfo.cpu",
            "Не удалось получить список CPU.",
        );
    }
    if report.memory.total.bytes == 0 {
        warn(
            &mut report,
            "sysinfo.memory",
            "Общий объём памяти недоступен; нули не считать реальной ёмкостью.",
        );
    }
    if report.disks.is_empty() {
        warn(&mut report, "sysinfo.disks", "Не получен список томов.");
    }
    if report.network.is_empty() {
        warn(
            &mut report,
            "sysinfo.network",
            "Не получен список сетевых интерфейсов.",
        );
    }
    if report.top_processes.is_empty() {
        warn(
            &mut report,
            "sysinfo.processes",
            "Не получен список процессов.",
        );
    }

    // A faulty WMI provider cannot block report delivery forever. Already received
    // sections survive a timeout. At most one hardware worker may exist at once.
    collect_hardware_with_deadline(&mut report);
    report.collection_duration_ms = start.elapsed().as_millis() as u64;
    Ok(report)
}

impl SystemReport {
    /// Clone the snapshot, not the live system. Local JSON remains unchanged.
    pub fn json_for_ai(&self, privacy: PrivacyOptions) -> Result<String> {
        let mut report = self.clone();
        report.privacy = PrivacyInfo {
            identifiers_redacted: privacy.hide_identifiers,
            process_list_omitted: !privacy.include_processes,
        };
        if privacy.hide_identifiers {
            report.os.hostname = None;
            for board in &mut report.motherboards {
                board.serial_number = None;
            }
            for bios in &mut report.bios {
                bios.serial_number = None;
            }
            for module in &mut report.memory.modules {
                module.serial_number = None;
            }
            for disk in &mut report.physical_disks {
                disk.serial_number = None;
            }
            for (i, disk) in report.disks.iter_mut().enumerate() {
                disk.name = format!("volume_{}", i + 1);
                disk.mount_point = format!("volume_{}", i + 1);
            }
            for (i, network) in report.network.iter_mut().enumerate() {
                network.interface = format!("interface_{}", i + 1);
                network.mac_address = None;
            }
            // Provider errors can contain local paths/hostnames. Do not transmit them.
            for warning in &mut report.warnings {
                warning.message = "Источник недоступен или вернул неполные данные. Подробности оставлены только в локальном отчёте.".into();
            }
        }
        if !privacy.include_processes {
            report.top_processes.clear();
        }
        serde_json::to_string_pretty(&report).context("Не удалось подготовить JSON для ИИ")
    }
}

enum HardwareEvent {
    Memory(Vec<MemoryModule>),
    Gpu(Vec<GpuInfo>),
    Dxgi(Vec<DxgiAdapterInfo>),
    Boards(Vec<MotherboardInfo>),
    Bios(Vec<BiosInfo>),
    Disks(Vec<PhysicalDiskInfo>),
    Cpu(Vec<CpuPackageInfo>),
    Warning(CollectionWarning),
    Finished,
}

struct HardwareBusyGuard;
impl Drop for HardwareBusyGuard {
    fn drop(&mut self) {
        HARDWARE_BUSY.store(false, Ordering::Release);
    }
}

fn collect_hardware_with_deadline(report: &mut SystemReport) {
    if HARDWARE_BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        warn(report, "hardware", "Предыдущий аппаратный запрос ещё не завершился. WMI/DXGI пропущены, чтобы не создавать зависшие потоки.");
        return;
    }
    let (tx, rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("hardware-wmi-dxgi".into())
        .spawn(move || {
            let _busy = HardwareBusyGuard;
            windows_hardware::collect(tx);
        });
    if let Err(error) = spawned {
        HARDWARE_BUSY.store(false, Ordering::Release);
        warn(
            report,
            "hardware",
            format!("Не удалось создать поток: {error}"),
        );
        return;
    }
    let deadline = Instant::now() + HARDWARE_TIMEOUT;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(HardwareEvent::Memory(v)) => report.memory.modules = v,
            Ok(HardwareEvent::Gpu(v)) => report.gpu = v,
            Ok(HardwareEvent::Dxgi(v)) => report.dxgi_adapters = v,
            Ok(HardwareEvent::Boards(v)) => report.motherboards = v,
            Ok(HardwareEvent::Bios(v)) => report.bios = v,
            Ok(HardwareEvent::Disks(v)) => report.physical_disks = v,
            Ok(HardwareEvent::Cpu(v)) => report.cpu.packages_wmi = v,
            Ok(HardwareEvent::Warning(v)) => report.warnings.push(v),
            Ok(HardwareEvent::Finished) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                warn(report, "hardware.timeout", "WMI/DXGI не завершились за 20 секунд. Показан частичный отчёт; уже полученные разделы сохранены. Системный вызов не прерывается принудительно.");
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                warn(
                    report,
                    "hardware.worker",
                    "Аппаратный поток завершился до окончания сбора. Показаны доступные разделы.",
                );
                break;
            }
        }
    }
}

#[cfg(windows)]
mod windows_hardware {
    use super::*;
    use anyhow::bail;
    use std::collections::HashMap;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND,
    };
    use wmi::{Variant, WMIConnection};

    type Row = HashMap<String, Variant>;

    fn text(row: &Row, key: &str) -> Option<String> {
        match row.get(key) {
            Some(Variant::String(value)) => {
                let value = value.trim().trim_matches('\0').trim();
                (!value.is_empty()).then(|| value.to_owned())
            }
            _ => None,
        }
    }

    fn unsigned(value: Option<&Variant>) -> Option<u64> {
        match value {
            Some(Variant::UI1(v)) => Some(u64::from(*v)),
            Some(Variant::UI2(v)) => Some(u64::from(*v)),
            Some(Variant::UI4(v)) => Some(u64::from(*v)),
            Some(Variant::UI8(v)) => Some(*v),
            Some(Variant::I1(v)) => u64::try_from(*v).ok(),
            Some(Variant::I2(v)) => u64::try_from(*v).ok(),
            Some(Variant::I4(v)) => u64::try_from(*v).ok(),
            Some(Variant::I8(v)) => u64::try_from(*v).ok(),
            Some(Variant::String(v)) => v.trim().parse::<u64>().ok(),
            _ => None,
        }
    }

    fn number(row: &Row, key: &str) -> Option<u64> {
        unsigned(row.get(key))
    }
    fn positive(row: &Row, key: &str) -> Option<u64> {
        number(row, key).and_then(nonzero)
    }
    fn size(row: &Row, key: &str) -> Option<Size> {
        positive(row, key).map(Size::from)
    }

    fn strings(row: &Row, key: &str) -> Vec<String> {
        match row.get(key) {
            Some(Variant::Array(values)) => values
                .iter()
                .filter_map(|v| match v {
                    Variant::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
                    _ => None,
                })
                .collect(),
            _ => text(row, key).into_iter().collect(),
        }
    }

    fn query<T>(connection: &WMIConnection, sql: &str, map: impl Fn(Row) -> T) -> Result<Vec<T>> {
        let rows: Vec<Row> = connection.raw_query(sql).context("Ошибка WMI-запроса")?;
        if rows.is_empty() {
            bail!("WMI-запрос не вернул объектов");
        }
        Ok(rows.into_iter().map(map).collect())
    }

    fn send<T>(
        tx: &mpsc::Sender<HardwareEvent>,
        component: &str,
        value: Result<Vec<T>>,
        event: impl FnOnce(Vec<T>) -> HardwareEvent,
    ) -> bool {
        let event = match value {
            Ok(value) => event(value),
            Err(error) => HardwareEvent::Warning(CollectionWarning {
                component: component.into(),
                message: format!("{error:#}"),
            }),
        };
        tx.send(event).is_ok()
    }

    pub(super) fn collect(tx: mpsc::Sender<HardwareEvent>) {
        if !send(&tx, "DXGI", collect_dxgi(), HardwareEvent::Dxgi) {
            return;
        }
        // wmi 0.18 initializes COM internally. No COMLibrary and no argument.
        // Never send WMIConnection over a channel: it is !Send and !Sync.
        let connection = match WMIConnection::new() {
            Ok(connection) => connection,
            Err(error) => {
                let _ = tx.send(HardwareEvent::Warning(CollectionWarning {
                    component: "WMI.connection".into(),
                    message: format!("Не удалось подключиться к WMI: {error}"),
                }));
                let _ = tx.send(HardwareEvent::Finished);
                return;
            }
        };
        let memory = query(&connection,
            "SELECT DeviceLocator, BankLabel, Manufacturer, PartNumber, SerialNumber, Capacity, Speed, ConfiguredClockSpeed, SMBIOSMemoryType, FormFactor, DataWidth, TotalWidth FROM Win32_PhysicalMemory",
            |r| MemoryModule {
                slot: text(&r, "DeviceLocator"), bank: text(&r, "BankLabel"),
                manufacturer: text(&r, "Manufacturer"), part_number: text(&r, "PartNumber"),
                serial_number: text(&r, "SerialNumber"), capacity: size(&r, "Capacity"),
                speed_mhz_reported: positive(&r, "Speed"),
                configured_clock_speed_mhz_reported: positive(&r, "ConfiguredClockSpeed"),
                smbios_memory_type_code: number(&r, "SMBIOSMemoryType"), form_factor_code: number(&r, "FormFactor"),
                data_width_bits: positive(&r, "DataWidth"), total_width_bits: positive(&r, "TotalWidth"),
            });
        if !send(&tx, "Win32_PhysicalMemory", memory, HardwareEvent::Memory) {
            return;
        }
        let gpu = query(&connection,
            "SELECT Name, AdapterCompatibility, AdapterRAM, DriverVersion, DriverDate, VideoProcessor, CurrentHorizontalResolution, CurrentVerticalResolution, CurrentRefreshRate, Status FROM Win32_VideoController",
            |r| GpuInfo {
                name: text(&r, "Name"), manufacturer: text(&r, "AdapterCompatibility"),
                video_processor: text(&r, "VideoProcessor"), driver_version: text(&r, "DriverVersion"),
                driver_date_wmi: text(&r, "DriverDate"), adapter_ram_wmi_reported: size(&r, "AdapterRAM"),
                adapter_ram_is_reliable: false,
                current_horizontal_resolution: positive(&r, "CurrentHorizontalResolution"),
                current_vertical_resolution: positive(&r, "CurrentVerticalResolution"),
                current_refresh_rate_hz_reported: positive(&r, "CurrentRefreshRate").filter(|v| *v > 1 && *v != u32::MAX as u64),
                status_reported: text(&r, "Status"),
            });
        if !send(&tx, "Win32_VideoController", gpu, HardwareEvent::Gpu) {
            return;
        }
        let boards = query(
            &connection,
            "SELECT Manufacturer, Product, Version, SerialNumber FROM Win32_BaseBoard",
            |r| MotherboardInfo {
                manufacturer: text(&r, "Manufacturer"),
                product: text(&r, "Product"),
                version: text(&r, "Version"),
                serial_number: text(&r, "SerialNumber"),
            },
        );
        if !send(&tx, "Win32_BaseBoard", boards, HardwareEvent::Boards) {
            return;
        }
        let bios = query(&connection,
            "SELECT Manufacturer, SMBIOSBIOSVersion, BIOSVersion, ReleaseDate, SerialNumber FROM Win32_BIOS",
            |r| BiosInfo {
                manufacturer: text(&r, "Manufacturer"), smbios_bios_version: text(&r, "SMBIOSBIOSVersion"),
                version_strings: strings(&r, "BIOSVersion"), release_date_wmi: text(&r, "ReleaseDate"),
                serial_number: text(&r, "SerialNumber"),
            });
        if !send(&tx, "Win32_BIOS", bios, HardwareEvent::Bios) {
            return;
        }
        let cpu = query(&connection,
            "SELECT Name, Manufacturer, SocketDesignation, NumberOfCores, NumberOfLogicalProcessors, CurrentClockSpeed, MaxClockSpeed, L2CacheSize, L3CacheSize FROM Win32_Processor",
            |r| CpuPackageInfo {
                name: text(&r, "Name"), manufacturer: text(&r, "Manufacturer"), socket: text(&r, "SocketDesignation"),
                physical_cores: positive(&r, "NumberOfCores"), logical_cores: positive(&r, "NumberOfLogicalProcessors"),
                current_clock_mhz_reported: positive(&r, "CurrentClockSpeed"), max_clock_mhz_reported: positive(&r, "MaxClockSpeed"),
                l2_cache_kib: number(&r, "L2CacheSize"), l3_cache_kib: number(&r, "L3CacheSize"),
            });
        if !send(&tx, "Win32_Processor", cpu, HardwareEvent::Cpu) {
            return;
        }
        let disks = query(&connection,
            "SELECT Model, Manufacturer, SerialNumber, FirmwareRevision, InterfaceType, MediaType, Size, Status FROM Win32_DiskDrive",
            |r| PhysicalDiskInfo {
                model: text(&r, "Model"), manufacturer: text(&r, "Manufacturer"), serial_number: text(&r, "SerialNumber"),
                firmware_revision: text(&r, "FirmwareRevision"), interface_type_reported: text(&r, "InterfaceType"),
                media_type_reported: text(&r, "MediaType"), size: size(&r, "Size"), status_reported: text(&r, "Status"),
            });
        if !send(&tx, "Win32_DiskDrive", disks, HardwareEvent::Disks) {
            return;
        }
        let _ = tx.send(HardwareEvent::Finished);
    }

    fn collect_dxgi() -> Result<Vec<DxgiAdapterInfo>> {
        // SAFETY: factory/adapter interfaces stay on this thread. windows-rs owns
        // their COM references; GetDesc1 returns an initialized value on success.
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.context("Не удалось создать фабрику DXGI")?;
        let mut result = Vec::new();
        for index in 0..128_u32 {
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => return Ok(result),
                Err(error) => return Err(error).context("Не удалось перечислить DXGI-адаптеры"),
            };
            let desc = unsafe { adapter.GetDesc1() }
                .context("Не удалось прочитать описание DXGI-адаптера")?;
            let len = desc
                .Description
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(desc.Description.len());
            result.push(DxgiAdapterInfo {
                name: String::from_utf16_lossy(&desc.Description[..len]),
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                dedicated_video_memory: (desc.DedicatedVideoMemory as u64).into(),
                dedicated_system_memory: (desc.DedicatedSystemMemory as u64).into(),
                shared_system_memory: (desc.SharedSystemMemory as u64).into(),
                is_software_adapter: desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
            });
        }
        bail!("DXGI вернул необычно много адаптеров (более 128)")
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn wmi_uint64_string_is_not_silently_zeroed() {
            assert_eq!(
                unsigned(Some(&Variant::String("17179869184".into()))),
                Some(17_179_869_184)
            );
        }
        #[test]
        fn wmi_negative_and_missing_numbers_are_unknown() {
            assert_eq!(unsigned(Some(&Variant::I4(-1))), None);
            assert_eq!(unsigned(Some(&Variant::Null)), None);
            assert_eq!(unsigned(None), None);
            assert_eq!(unsigned(Some(&Variant::UI8(0))), Some(0));
        }
    }
}

#[cfg(not(windows))]
mod windows_hardware {
    use super::*;
    pub(super) fn collect(tx: mpsc::Sender<HardwareEvent>) {
        let _ = tx.send(HardwareEvent::Warning(CollectionWarning {
            component: "platform".into(),
            message: "WMI/DXGI доступны только на Windows.".into(),
        }));
        let _ = tx.send(HardwareEvent::Finished);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(Size::from(17_179_869_184).gib, 16.0);
    }
    #[test]
    fn redaction_does_not_modify_local_report() {
        let mut r = SystemReport::default();
        r.os.hostname = Some("private-host".into());
        r.bios.push(BiosInfo {
            serial_number: Some("serial-secret".into()),
            ..Default::default()
        });
        r.memory.modules.push(MemoryModule {
            serial_number: Some("ram-secret".into()),
            ..Default::default()
        });
        r.network.push(NetworkInfo {
            interface: "private-interface".into(),
            mac_address: Some("AA:BB:CC:DD:EE:FF".into()),
            ..Default::default()
        });
        r.disks.push(DiskInfo {
            name: "private-volume".into(),
            mount_point: "C:\\Users\\private".into(),
            ..Default::default()
        });
        r.top_processes.push(ProcessInfo {
            name: "private-process".into(),
            ..Default::default()
        });
        r.warnings.push(CollectionWarning {
            component: "WMI".into(),
            message: "private-error".into(),
        });
        let redacted = r.json_for_ai(PrivacyOptions::default()).unwrap();
        for private in [
            "private-host",
            "serial-secret",
            "ram-secret",
            "private-interface",
            "AA:BB",
            "private-volume",
            "private-process",
            "private-error",
        ] {
            assert!(!redacted.contains(private), "leaked: {private}");
        }
        assert_eq!(r.os.hostname.as_deref(), Some("private-host"));
        assert_eq!(r.top_processes.len(), 1);
        let full = r
            .json_for_ai(PrivacyOptions {
                hide_identifiers: false,
                include_processes: true,
            })
            .unwrap();
        assert!(full.contains("private-process"));
        assert!(full.contains("private-host"));
    }
    #[test]
    fn sorting_memory_does_not_need_float_comparisons() {
        let mut p = [
            ProcessInfo {
                pid: 2,
                memory_bytes: 42,
                ..Default::default()
            },
            ProcessInfo {
                pid: 1,
                memory_bytes: 42,
                ..Default::default()
            },
        ];
        p.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes).then(a.pid.cmp(&b.pid)));
        assert_eq!(p[0].pid, 1);
    }
}
