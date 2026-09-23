#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod collector;

use anyhow::{anyhow, bail, Context, Result};
use collector::{
    BiosInfo, CpuPackageInfo, DiskInfo, DxgiAdapterInfo, GpuInfo, MemoryModule, MotherboardInfo,
    NetworkInfo, PhysicalDiskInfo, PrivacyOptions, Size, SystemReport,
};
use egui::{Align, Color32, Layout, RichText, Stroke};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

macro_rules! t {
    ($lang:expr, $ru:expr, $en:expr) => {
        if matches!($lang, Language::Ru) {
            $ru
        } else {
            $en
        }
    };
}

fn main() {
    if let Err(error) = run() {
        show_startup_error(&format!(
            "Не удалось запустить SysInfo AI / Failed to start SysInfo AI:\n{error}\n\nПроверьте драйвер видеокарты: интерфейс использует OpenGL.\nCheck the GPU driver: the UI uses OpenGL."
        ));
    }
}

fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1220.0, 860.0])
            .with_min_inner_size([980.0, 700.0]),
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "SysInfo AI",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(windows)]
fn show_startup_error(message: &str) {
    use windows::{
        core::{w, PCWSTR},
        Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK},
    };
    let wide: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(wide.as_ptr()),
            w!("SysInfo AI"),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn show_startup_error(message: &str) {
    eprintln!("{message}");
}

struct Job<T> {
    receiver: Receiver<Result<T>>,
    started: Instant,
}

fn spawn_job<T, F>(name: &str, ctx: &egui::Context, work: F) -> Result<Job<T>>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let repaint = ctx.clone();
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|_| {
                Err(anyhow!(
                    "Фоновая операция завершилась внутренней ошибкой (panic)."
                ))
            });
            let _ = sender.send(result);
            repaint.request_repaint();
        })
        .context("Не удалось запустить фоновую операцию")?;
    Ok(Job {
        receiver,
        started: Instant::now(),
    })
}

fn poll_job<T>(job: &mut Option<Job<T>>) -> Option<Result<T>> {
    let result = match job.as_ref()?.receiver.try_recv() {
        Ok(value) => value,
        Err(TryRecvError::Empty) => return None,
        Err(TryRecvError::Disconnected) => Err(anyhow!("Связь с фоновой операцией потеряна.")),
    };
    *job = None;
    Some(result)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Language {
    Ru,
    En,
}

impl From<Language> for ai::AnalysisLanguage {
    fn from(language: Language) -> Self {
        match language {
            Language::Ru => Self::Russian,
            Language::En => Self::English,
        }
    }
}

impl From<ai::AnalysisLanguage> for Language {
    fn from(language: ai::AnalysisLanguage) -> Self {
        match language {
            ai::AnalysisLanguage::Russian => Self::Ru,
            ai::AnalysisLanguage::English => Self::En,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeMode {
    Light,
    Dark,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    System,
    Ai,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SystemSection {
    Overview,
    Os,
    Cpu,
    Memory,
    Storage,
    Gpu,
    Board,
    Network,
    Processes,
    Json,
}

struct Snapshot {
    report: SystemReport,
    json: String,
}

struct CompletedAnalysis {
    result: ai::AnalysisResult,
    generation: u64,
    timestamp: u64,
    privacy: PrivacyOptions,
    language: Language,
}

struct PendingSend {
    request: ai::AnalysisRequest,
    generation: u64,
    timestamp: u64,
    privacy: PrivacyOptions,
    confirmed: bool,
}

struct PendingSave {
    title: String,
    contents: String,
    path: String,
    contains_identifiers: bool,
}

enum Dialog {
    Send(PendingSend),
    Save(PendingSave),
}

struct App {
    tab: Tab,
    language: Language,
    theme: ThemeMode,
    system_section: SystemSection,
    snapshot: Option<Snapshot>,
    generation: u64,
    collect_job: Option<Job<Snapshot>>,
    analysis_job: Option<Job<CompletedAnalysis>>,
    // Captured when the confirmed job starts, not read from the live UI later.
    analysis_job_language: Option<ai::AnalysisLanguage>,
    models_job: Option<Job<Vec<ai::ModelOption>>>,
    save_job: Option<Job<PathBuf>>,
    api_key: String,
    show_key: bool,
    models: Vec<ai::ModelOption>,
    catalog_loaded: bool,
    model_id: String,
    manual_model: bool,
    model_filter: String,
    privacy: PrivacyOptions,
    timeout_seconds: u64,
    max_tokens: u32,
    deny_data_collection: bool,
    analysis: String,
    analysis_metadata: Option<CompletedAnalysis>,
    dialog: Option<Dialog>,
    errors: Vec<String>,
    notice: Option<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let language = Language::Ru;
        let theme = ThemeMode::Light;
        apply_theme(&cc.egui_ctx, theme);
        apply_text_style(&cc.egui_ctx);

        let models = ai::builtin_models();
        let mut app = Self {
            tab: Tab::System,
            language,
            theme,
            system_section: SystemSection::Overview,
            snapshot: None,
            generation: 0,
            collect_job: None,
            analysis_job: None,
            analysis_job_language: None,
            models_job: None,
            save_job: None,
            api_key: String::new(),
            show_key: false,
            model_id: models.first().map(|m| m.id.clone()).unwrap_or_default(),
            models,
            catalog_loaded: false,
            manual_model: false,
            model_filter: String::new(),
            privacy: PrivacyOptions::default(),
            timeout_seconds: 180,
            max_tokens: 3072,
            deny_data_collection: true,
            analysis: String::new(),
            analysis_metadata: None,
            dialog: None,
            errors: Vec::new(),
            notice: None,
        };
        app.start_collection(&cc.egui_ctx);
        app
    }

    fn error(&mut self, error: anyhow::Error) {
        self.errors.push(format!("{error:#}"));
        if self.errors.len() > 8 {
            self.errors.remove(0);
        }
    }

    fn switch_language(&mut self, language: Language) {
        self.language = language;
    }

    fn switch_theme(&mut self, ctx: &egui::Context, theme: ThemeMode) {
        self.theme = theme;
        apply_theme(ctx, theme);
        apply_text_style(ctx);
    }

    fn start_collection(&mut self, ctx: &egui::Context) {
        if self.collect_job.is_some() {
            return;
        }
        match spawn_job("system-collector", ctx, || {
            let report = collector::collect_all()?;
            let json =
                serde_json::to_string_pretty(&report).context("Не удалось сериализовать отчёт")?;
            Ok(Snapshot { report, json })
        }) {
            Ok(job) => self.collect_job = Some(job),
            Err(error) => self.error(error),
        }
    }

    fn poll(&mut self) {
        if let Some(result) = poll_job(&mut self.collect_job) {
            match result {
                Ok(snapshot) => {
                    self.generation += 1;
                    self.notice = Some(if matches!(self.language, Language::Ru) {
                        format!(
                            "Снимок №{} собран. Предупреждений: {}.",
                            self.generation,
                            snapshot.report.warnings.len()
                        )
                    } else {
                        format!(
                            "Snapshot #{} has been collected. Warnings: {}.",
                            self.generation,
                            snapshot.report.warnings.len()
                        )
                    });
                    self.snapshot = Some(snapshot);
                }
                Err(error) => self.error(error.context("Сбор данных / Data collection")),
            }
        }
        if let Some(result) = poll_job(&mut self.models_job) {
            match result {
                Ok(models) => {
                    self.notice = Some(if matches!(self.language, Language::Ru) {
                        format!(
                            "Каталог загружен: {} текстовых моделей. Текущий ID модели сохранён.",
                            models.len()
                        )
                    } else {
                        format!(
                            "Catalog loaded: {} text models. The current model ID has been preserved.",
                            models.len()
                        )
                    });
                    self.models = models;
                    self.catalog_loaded = true;
                }
                Err(error) => self.error(error.context(t!(
                    self.language,
                    "Загрузка каталога моделей",
                    "Loading the model catalog"
                ))),
            }
        }
        if let Some(result) = poll_job(&mut self.analysis_job) {
            self.analysis_job_language = None;
            match result {
                Ok(completed) => {
                    self.analysis = format_analysis(&completed);
                    self.analysis_metadata = Some(completed);
                    self.notice = Some(
                        t!(
                        self.language,
                        "ИИ-анализ получен. Его можно отредактировать, скопировать или сохранить.",
                        "AI analysis has been received. You can edit, copy, or save it."
                    )
                        .to_owned(),
                    );
                }
                Err(error) => self.error(error.context(t!(
                    self.language,
                    "ИИ-анализ; предыдущий результат, если он был, сохранён",
                    "AI analysis failed; the previous result, if any, was kept"
                ))),
            }
        }
        if let Some(result) = poll_job(&mut self.save_job) {
            match result {
                Ok(path) => {
                    self.notice = Some(if matches!(self.language, Language::Ru) {
                        format!("Сохранено: {}", path.display())
                    } else {
                        format!("Saved: {}", path.display())
                    });
                }
                Err(error) => self.error(error.context("Сохранение файла / File saving")),
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(RichText::new("SysInfo AI").size(30.0).strong());
                ui.label(
                    RichText::new(t!(
                        self.language,
                        "Нативный Windows-инвентарь и понятное объяснение конфигурации",
                        "Native Windows inventory and human-friendly PC explanation"
                    ))
                    .small()
                    .weak(),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(RichText::new(t!(self.language, "Тема", "Theme")).small());
                if ui
                    .selectable_label(
                        matches!(self.theme, ThemeMode::Dark),
                        t!(self.language, "Тёмная", "Dark"),
                    )
                    .clicked()
                {
                    self.switch_theme(ctx, ThemeMode::Dark);
                }
                if ui
                    .selectable_label(
                        matches!(self.theme, ThemeMode::Light),
                        t!(self.language, "Светлая", "Light"),
                    )
                    .clicked()
                {
                    self.switch_theme(ctx, ThemeMode::Light);
                }
                ui.separator();
                ui.label(RichText::new(t!(self.language, "Язык", "Language")).small());
                if ui
                    .selectable_label(matches!(self.language, Language::En), "EN")
                    .clicked()
                {
                    self.switch_language(Language::En);
                }
                if ui
                    .selectable_label(matches!(self.language, Language::Ru), "RU")
                    .clicked()
                {
                    self.switch_language(Language::Ru);
                }
            });
        });
        ui.add_space(8.0);
        let navigation_frame = egui::Frame::group(ui.style())
            .fill(panel_fill(ui))
            .stroke(Stroke::new(
                1.0,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .inner_margin(egui::Margin::same(10));
        full_width_frame(ui, navigation_frame, |ui| {
            ui.horizontal_wrapped(|ui| {
                tab_button(
                    ui,
                    &mut self.tab,
                    Tab::System,
                    "",
                    t!(self.language, "Система", "System"),
                );
                tab_button(
                    ui,
                    &mut self.tab,
                    Tab::Ai,
                    "",
                    t!(self.language, "ИИ-анализ", "AI analysis"),
                );
            });
        });
        ui.add_space(4.0);
    }

    fn system_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical()
            .id_salt("system-tab-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut save = false;
                card(ui, "", t!(self.language, "Управление отчётом", "Report actions"), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(
                                self.collect_job.is_none(),
                                egui::Button::new(t!(self.language, "Обновить данные", "Refresh data")),
                            )
                            .clicked()
                        {
                            self.start_collection(ctx);
                        }
                        if ui
                            .add_enabled(
                                self.snapshot.is_some(),
                                egui::Button::new(t!(self.language, "Скопировать JSON", "Copy JSON")),
                            )
                            .clicked()
                        {
                            if let Some(snapshot) = &self.snapshot {
                                ctx.copy_text(snapshot.json.clone());
                            }
                            self.notice = Some(
                                t!(
                                    self.language,
                                    "Полный JSON скопирован в буфер обмена.",
                                    "The full JSON has been copied to the clipboard."
                                )
                                .to_owned(),
                            );
                        }
                        save = ui
                            .add_enabled(
                                self.snapshot.is_some() && self.save_job.is_none(),
                                egui::Button::new(t!(self.language, "Сохранить в файл", "Save to file")),
                            )
                            .clicked();
                        if let Some(job) = &self.collect_job {
                            ui.spinner();
                            ui.label(if matches!(self.language, Language::Ru) {
                                format!("Сбор данных · {} с", job.started.elapsed().as_secs())
                            } else {
                                format!("Collecting data · {} s", job.started.elapsed().as_secs())
                            });
                        }
                    });
                });

                if save {
                    if let Some(snapshot) = &self.snapshot {
                        self.dialog = Some(Dialog::Save(PendingSave {
                            title: t!(self.language, "Сохранить системный отчёт", "Save system report")
                                .to_owned(),
                            contents: snapshot.json.clone(),
                            path: default_save_path("system_report.json"),
                            contains_identifiers: true,
                        }));
                    }
                }

                let Some(snapshot) = &self.snapshot else {
                    empty_state(
                        ui,
                        "",
                        t!(self.language, "Здесь появится конфигурация компьютера", "Your computer configuration will appear here"),
                        t!(self.language, "Локальный сбор уже запущен. Для этого интернет не нужен.", "A local scan has already started. Internet access is not required."),
                    );
                    return;
                };

                let report = &snapshot.report;

                ui.add_space(6.0);
                if self.system_section == SystemSection::Overview {
                    render_summary_cards(ui, self.language, report);
                    ui.add_space(8.0);
                }

                card(ui, "", t!(self.language, "Информация о снимке", "Snapshot info"), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        badge(
                            ui,
                            Color32::from_rgb(92, 135, 245),
                            if matches!(self.language, Language::Ru) {
                                format!(
                                    "Снимок №{} • {} мс • Unix UTC {}",
                                    self.generation, report.collection_duration_ms, report.collected_at_unix_seconds
                                )
                            } else {
                                format!(
                                    "Snapshot #{} • {} ms • Unix UTC {}",
                                    self.generation, report.collection_duration_ms, report.collected_at_unix_seconds
                                )
                            },
                        );
                        badge(
                            ui,
                            Color32::from_rgb(214, 138, 58),
                            if matches!(self.language, Language::Ru) {
                                format!("Предупреждения: {}", report.warnings.len())
                            } else {
                                format!("Warnings: {}", report.warnings.len())
                            },
                        );
                    });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(t!(
                            self.language,
                            "Полный локальный JSON содержит hostname, MAC, серийные номера и процессы. Для ИИ используется отдельная настраиваемая копия отчёта.",
                            "The full local JSON contains hostname, MAC addresses, serial numbers, and processes. AI uses a separate configurable copy of the report."
                        ))
                        .small()
                        .color(Color32::from_rgb(214, 138, 58)),
                    );
                });

                if !report.warnings.is_empty() {
                    card(ui, "", t!(self.language, "Предупреждения", "Warnings"), |ui| {
                        for warning in &report.warnings {
                            ui.add(egui::Label::new(format!(
                                "{}: {}", warning.component, warning.message
                            )).wrap());
                        }
                    });
                }

                ui.add_space(6.0);
                section_selector(ui, self.language, &mut self.system_section);
                ui.add_space(8.0);

                match self.system_section {
                    SystemSection::Overview => self.system_overview(ui, report),
                    SystemSection::Os => self.os_section(ui, report),
                    SystemSection::Cpu => self.cpu_section(ui, report),
                    SystemSection::Memory => self.memory_section(ui, report),
                    SystemSection::Storage => self.storage_section(ui, report),
                    SystemSection::Gpu => self.gpu_section(ui, report),
                    SystemSection::Board => self.board_section(ui, report),
                    SystemSection::Network => self.network_section(ui, report),
                    SystemSection::Processes => self.processes_section(ui, report),
                    SystemSection::Json => self.json_section(ui, snapshot),
                }
                ui.add_space(8.0);
            });
    }

    fn system_overview(&self, ui: &mut egui::Ui, report: &SystemReport) {
        responsive_cards(ui, "overview-cards", 2, |ui, index| {
            if index == 0 {
                card(
                    ui,
                    "",
                    t!(self.language, "Ключевые сведения", "Key facts"),
                    |ui| {
                        key_value(
                            ui,
                            t!(self.language, "Материнская плата", "Motherboard"),
                            report
                                .motherboards
                                .first()
                                .and_then(|b| b.product.as_deref())
                                .or_else(|| {
                                    report
                                        .motherboards
                                        .first()
                                        .and_then(|b| b.manufacturer.as_deref())
                                })
                                .unwrap_or("—"),
                        );
                        key_value(
                            ui,
                            t!(self.language, "BIOS", "BIOS"),
                            report
                                .bios
                                .first()
                                .and_then(|b| b.smbios_bios_version.as_deref())
                                .unwrap_or("—"),
                        );
                        key_value(
                            ui,
                            t!(self.language, "Планок RAM", "RAM modules"),
                            &report.memory.modules.len().to_string(),
                        );
                        key_value(
                            ui,
                            t!(self.language, "Сетевых интерфейсов", "Network interfaces"),
                            &report.network.len().to_string(),
                        );
                        key_value(
                            ui,
                            t!(self.language, "Топ процессов", "Top processes"),
                            &report.top_processes.len().to_string(),
                        );
                    },
                );
            } else {
                card(
                    ui,
                    "",
                    t!(self.language, "Хранилище и графика", "Storage and graphics"),
                    |ui| {
                        key_value(
                            ui,
                            t!(self.language, "Тома", "Volumes"),
                            &report.disks.len().to_string(),
                        );
                        key_value(
                            ui,
                            t!(self.language, "Физические диски", "Physical disks"),
                            &report.physical_disks.len().to_string(),
                        );
                        key_value(
                            ui,
                            t!(self.language, "GPU через WMI", "GPUs via WMI"),
                            &report.gpu.len().to_string(),
                        );
                        key_value(
                            ui,
                            t!(self.language, "DXGI-адаптеры", "DXGI adapters"),
                            &report.dxgi_adapters.len().to_string(),
                        );
                        let total_storage: u64 = report.disks.iter().map(|d| d.total.bytes).sum();
                        key_value(
                            ui,
                            t!(
                                self.language,
                                "Суммарный объём томов",
                                "Total volume capacity"
                            ),
                            &format_bytes(total_storage),
                        );
                    },
                );
            }
        });

        card(
            ui,
            "",
            t!(self.language, "Ограничения данных", "Data limitations"),
            |ui| {
                // Natural content height; the enclosing page supplies the scroll bar.
                for limitation in &report.limitations {
                    ui.add(egui::Label::new(format!("• {limitation}")).wrap());
                }
            },
        );
    }

    fn os_section(&self, ui: &mut egui::Ui, report: &SystemReport) {
        card(
            ui,
            "🪟",
            t!(self.language, "Операционная система", "Operating system"),
            |ui| {
                two_column_grid(ui, "os-grid", |ui| {
                    key_value_row(
                        ui,
                        t!(self.language, "Название", "Name"),
                        report.os.name.as_deref().unwrap_or("—"),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Версия", "Version"),
                        report.os.version.as_deref().unwrap_or("—"),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Подробная версия", "Detailed version"),
                        report.os.long_version.as_deref().unwrap_or("—"),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Версия ядра", "Kernel version"),
                        report.os.kernel_version.as_deref().unwrap_or("—"),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Архитектура", "Architecture"),
                        &report.os.architecture,
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Hostname", "Hostname"),
                        report.os.hostname.as_deref().unwrap_or("—"),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Uptime", "Uptime"),
                        &fmt_uptime(report.os.uptime_seconds, self.language),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Boot time Unix", "Boot time Unix"),
                        &report.os.boot_time_unix_seconds.to_string(),
                    );
                });
            },
        );
    }

    fn cpu_section(&self, ui: &mut egui::Ui, report: &SystemReport) {
        card(
            ui,
            "🧠",
            t!(self.language, "Центральный процессор", "Central processor"),
            |ui| {
                two_column_grid(ui, "cpu-grid", |ui| {
                    key_value_row(
                        ui,
                        t!(self.language, "Модель", "Model"),
                        report.cpu.brand.as_deref().unwrap_or("—"),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Физические ядра", "Physical cores"),
                        &report
                            .cpu
                            .physical_cores
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "—".into()),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Логические ядра", "Logical cores"),
                        &report.cpu.logical_cores.to_string(),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Частота", "Reported frequency"),
                        &opt_u64(report.cpu.reported_frequency_mhz, "MHz"),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Загрузка CPU", "CPU usage"),
                        &opt_f32_percent(report.cpu.usage_percent),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Интервал замера", "Sample interval"),
                        &format!("{} ms", report.cpu_sample_interval_ms),
                    );
                });
            },
        );

        if !report.cpu.packages_wmi.is_empty() {
            card(
                ui,
                "📦",
                t!(
                    self.language,
                    "Сокеты / пакеты CPU (WMI)",
                    "CPU packages / sockets (WMI)"
                ),
                |ui| {
                    responsive_cards(
                        ui,
                        "cpu-packages",
                        report.cpu.packages_wmi.len(),
                        |ui, index| {
                            let package = &report.cpu.packages_wmi[index];
                            subcard(
                                ui,
                                &format!(
                                    "{} #{}",
                                    t!(self.language, "Пакет", "Package"),
                                    index + 1
                                ),
                                |ui| {
                                    render_cpu_package(ui, self.language, package);
                                },
                            );
                        },
                    );
                },
            );
        }

        card(
            ui,
            "🧵",
            t!(self.language, "Логические процессоры", "Logical processors"),
            |ui| {
                data_grid(ui, "logical-cpus-grid", 5, [12.0, 6.0], |ui| {
                    header_cell(ui, t!(self.language, "Имя", "Name"));
                    header_cell(ui, t!(self.language, "Бренд", "Brand"));
                    header_cell(ui, t!(self.language, "Vendor", "Vendor"));
                    header_cell(ui, t!(self.language, "МГц", "MHz"));
                    header_cell(ui, t!(self.language, "%", "%"));
                    ui.end_row();
                    for cpu in &report.cpu.logical_processors {
                        ui.label(&cpu.name);
                        ui.label(&cpu.brand);
                        ui.label(&cpu.vendor_id);
                        ui.label(opt_u64(cpu.reported_frequency_mhz, ""));
                        ui.label(opt_f32_percent(cpu.usage_percent));
                        ui.end_row();
                    }
                });
            },
        );
    }

    fn memory_section(&self, ui: &mut egui::Ui, report: &SystemReport) {
        card(
            ui,
            "💾",
            t!(self.language, "Оперативная память", "Memory"),
            |ui| {
                two_column_grid(ui, "memory-grid", |ui| {
                    key_value_row(
                        ui,
                        t!(self.language, "Всего RAM", "Total RAM"),
                        &fmt_size(report.memory.total),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Использовано", "Used"),
                        &fmt_size(report.memory.used),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Доступно", "Available"),
                        &fmt_size(report.memory.available),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Свободно", "Free"),
                        &fmt_size(report.memory.free),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Использование", "Usage"),
                        &opt_f64_percent(report.memory.usage_percent),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Swap всего", "Total swap"),
                        &fmt_size(report.memory.swap_total),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Swap занято", "Swap used"),
                        &fmt_size(report.memory.swap_used),
                    );
                    key_value_row(
                        ui,
                        t!(self.language, "Swap свободно", "Swap free"),
                        &fmt_size(report.memory.swap_free),
                    );
                });
            },
        );

        card(
            ui,
            "🧩",
            t!(self.language, "Планки памяти (WMI)", "Memory modules (WMI)"),
            |ui| {
                if report.memory.modules.is_empty() {
                    ui.label(t!(
                        self.language,
                        "Нет данных о планках памяти.",
                        "No per-module memory information is available."
                    ));
                } else {
                    responsive_cards(
                        ui,
                        "memory-modules",
                        report.memory.modules.len(),
                        |ui, index| {
                            let module = &report.memory.modules[index];
                            subcard(
                                ui,
                                &format!(
                                    "{} #{}",
                                    t!(self.language, "Планка", "Module"),
                                    index + 1
                                ),
                                |ui| {
                                    render_memory_module(ui, self.language, module);
                                },
                            );
                        },
                    );
                }
            },
        );
    }

    fn storage_section(&self, ui: &mut egui::Ui, report: &SystemReport) {
        card(
            ui,
            "🗄",
            t!(
                self.language,
                "Тома / логические диски",
                "Volumes / logical disks"
            ),
            |ui| {
                if report.disks.is_empty() {
                    ui.label(t!(
                        self.language,
                        "Список томов пуст.",
                        "The volumes list is empty."
                    ));
                } else {
                    responsive_cards(ui, "volumes", report.disks.len(), |ui, index| {
                        let disk = &report.disks[index];
                        subcard(ui, &disk.mount_point, |ui| {
                            render_volume(ui, self.language, disk)
                        });
                    });
                }
            },
        );

        card(
            ui,
            "💽",
            t!(
                self.language,
                "Физические накопители (WMI)",
                "Physical storage devices (WMI)"
            ),
            |ui| {
                if report.physical_disks.is_empty() {
                    ui.label(t!(
                        self.language,
                        "Список физических дисков пуст.",
                        "The physical disk list is empty."
                    ));
                } else {
                    responsive_cards(
                        ui,
                        "physical-disks",
                        report.physical_disks.len(),
                        |ui, index| {
                            let disk = &report.physical_disks[index];
                            subcard(
                                ui,
                                &format!("{} #{}", t!(self.language, "Диск", "Disk"), index + 1),
                                |ui| {
                                    render_physical_disk(ui, self.language, disk);
                                },
                            );
                        },
                    );
                }
            },
        );
    }

    fn gpu_section(&self, ui: &mut egui::Ui, report: &SystemReport) {
        card(
            ui,
            "🎮",
            t!(self.language, "GPU через WMI", "GPU via WMI"),
            |ui| {
                if report.gpu.is_empty() {
                    ui.label(t!(
                        self.language,
                        "Список GPU пуст.",
                        "The GPU list is empty."
                    ));
                } else {
                    responsive_cards(ui, "wmi-gpus", report.gpu.len(), |ui, index| {
                        let gpu = &report.gpu[index];
                        subcard(
                            ui,
                            &format!("{} #{}", t!(self.language, "GPU", "GPU"), index + 1),
                            |ui| {
                                render_gpu(ui, self.language, gpu);
                            },
                        );
                    });
                }
            },
        );

        card(
            ui,
            "📊",
            t!(
                self.language,
                "DXGI-адаптеры и память",
                "DXGI adapters and memory"
            ),
            |ui| {
                if report.dxgi_adapters.is_empty() {
                    ui.label(t!(
                        self.language,
                        "Список DXGI-адаптеров пуст.",
                        "The DXGI adapter list is empty."
                    ));
                } else {
                    responsive_cards(
                        ui,
                        "dxgi-adapters",
                        report.dxgi_adapters.len(),
                        |ui, index| {
                            let adapter = &report.dxgi_adapters[index];
                            subcard(ui, &adapter.name, |ui| {
                                render_dxgi(ui, self.language, adapter)
                            });
                        },
                    );
                }
            },
        );
    }

    fn board_section(&self, ui: &mut egui::Ui, report: &SystemReport) {
        card(
            ui,
            "🧱",
            t!(self.language, "Материнская плата", "Motherboard"),
            |ui| {
                if report.motherboards.is_empty() {
                    ui.label(t!(
                        self.language,
                        "Нет данных о материнской плате.",
                        "No motherboard data is available."
                    ));
                } else {
                    responsive_cards(
                        ui,
                        "motherboards",
                        report.motherboards.len(),
                        |ui, index| {
                            let board = &report.motherboards[index];
                            subcard(
                                ui,
                                &format!("{} #{}", t!(self.language, "Плата", "Board"), index + 1),
                                |ui| {
                                    render_board(ui, self.language, board);
                                },
                            );
                        },
                    );
                }
            },
        );

        card(ui, "📟", t!(self.language, "BIOS", "BIOS"), |ui| {
            if report.bios.is_empty() {
                ui.label(t!(
                    self.language,
                    "Нет данных о BIOS.",
                    "No BIOS data is available."
                ));
            } else {
                responsive_cards(ui, "bios-devices", report.bios.len(), |ui, index| {
                    let bios = &report.bios[index];
                    subcard(
                        ui,
                        &format!("{} #{}", t!(self.language, "BIOS", "BIOS"), index + 1),
                        |ui| {
                            render_bios(ui, self.language, bios);
                        },
                    );
                });
            }
        });
    }

    fn network_section(&self, ui: &mut egui::Ui, report: &SystemReport) {
        card(
            ui,
            "🌐",
            t!(self.language, "Сетевые интерфейсы", "Network interfaces"),
            |ui| {
                if report.network.is_empty() {
                    ui.label(t!(
                        self.language,
                        "Список сетевых интерфейсов пуст.",
                        "The network interface list is empty."
                    ));
                } else {
                    responsive_cards(
                        ui,
                        "network-interfaces",
                        report.network.len(),
                        |ui, index| {
                            let network = &report.network[index];
                            subcard(ui, &network.interface, |ui| {
                                render_network(ui, self.language, network)
                            });
                        },
                    );
                }
            },
        );
    }

    fn processes_section(&self, ui: &mut egui::Ui, report: &SystemReport) {
        card(
            ui,
            "📈",
            t!(
                self.language,
                "Топ-10 процессов по памяти",
                "Top 10 processes by memory"
            ),
            |ui| {
                if report.top_processes.is_empty() {
                    ui.label(t!(
                        self.language,
                        "Список процессов пуст.",
                        "The process list is empty."
                    ));
                } else {
                    data_grid(ui, "processes-grid", 5, [12.0, 8.0], |ui| {
                        header_cell(ui, t!(self.language, "PID", "PID"));
                        header_cell(ui, t!(self.language, "Процесс", "Process"));
                        header_cell(ui, t!(self.language, "Память", "Memory"));
                        header_cell(ui, t!(self.language, "MiB", "MiB"));
                        header_cell(ui, t!(self.language, "CPU %", "CPU %"));
                        ui.end_row();
                        for process in &report.top_processes {
                            ui.label(process.pid.to_string());
                            ui.label(&process.name);
                            ui.label(format_bytes(process.memory_bytes));
                            ui.label(format!("{:.1}", process.memory_mib));
                            ui.label(opt_f32_percent(process.cpu_percent_of_system));
                            ui.end_row();
                        }
                    });
                }
            },
        );
    }

    fn json_section(&self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        card(
            ui,
            "{ }",
            t!(self.language, "Полный JSON-отчёт", "Full JSON report"),
            |ui| {
                ui.label(
                RichText::new(t!(
                    self.language,
                    "Этот блок предназначен для точного просмотра локального отчёта, копирования и сохранения. Он остаётся неизменным, даже если ИИ получает отдельную приватную копию.",
                    "This block is meant for precise local report inspection, copying, and saving. It remains unchanged even when AI receives a separate privacy-filtered copy."
                ))
                .small(),
            );
                // Only horizontal scrolling for long JSON lines. Vertical scrolling
                // belongs to system-tab-scroll, so the full document contributes height.
                egui::ScrollArea::horizontal()
                    .id_salt("system-json-scroll")
                    .auto_shrink([false, true])
                    .show(ui, |ui| selectable_json(ui, "system-json", &snapshot.json));
            },
        );
    }

    fn ai_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical()
            .id_salt("ai-tab-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                card(ui, "🤖", t!(self.language, "ИИ-анализ конфигурации", "AI configuration analysis"), |ui| {
                    ui.label(t!(
                        self.language,
                        "Модель получит выбранный JSON и объяснит конфигурацию простым языком: сильные стороны, возможные узкие места и практические рекомендации.",
                        "The model will receive the selected JSON and explain the configuration in plain language: strengths, possible bottlenecks, and practical recommendations."
                    ));
                });

                card(ui, "🔐", t!(self.language, "OpenRouter и модель", "OpenRouter and model"), |ui| {
                    ui.label(RichText::new(t!(self.language, "API-ключ OpenRouter", "OpenRouter API key")).strong());
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.api_key)
                                .id_salt("openrouter-key")
                                .password(!self.show_key)
                                .desired_width(500.0)
                                .char_limit(512)
                                .hint_text(t!(self.language, "Вставьте API-ключ", "Paste the API key")),
                        );
                        ui.checkbox(&mut self.show_key, t!(self.language, "Показать", "Show"));
                        if ui.button(t!(self.language, "Очистить", "Clear")).clicked() {
                            self.api_key.clear();
                            self.show_key = false;
                        }
                    });
                    ui.label(
                        RichText::new(t!(
                            self.language,
                            "Ключ не сохраняется на диск. После отправки поле очищается — для нового запроса его нужно ввести снова.",
                            "The key is not saved to disk. After a confirmed request the field is cleared, so you must enter it again for the next request."
                        ))
                        .small()
                        .weak(),
                    );
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(t!(self.language, "Модель", "Model")).strong());
                        ui.checkbox(&mut self.manual_model, t!(self.language, "Ввести ID вручную", "Enter model ID manually"));
                        if ui
                            .add_enabled(
                                self.models_job.is_none(),
                                egui::Button::new(t!(self.language, "Загрузить каталог (интернет)", "Load catalog (internet)")),
                            )
                            .clicked()
                        {
                            let language = ai::AnalysisLanguage::from(self.language);
                            match spawn_job("openrouter-models", ctx, move || ai::fetch_models(language)) {
                                Ok(job) => self.models_job = Some(job),
                                Err(error) => self.error(error),
                            }
                        }
                        if self.models_job.is_some() {
                            ui.spinner();
                        }
                    });
                    if self.manual_model {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.model_id)
                                .desired_width(f32::INFINITY)
                                .char_limit(256)
                                .hint_text("provider/model-id"),
                        );
                    } else {
                        ui.horizontal(|ui| {
                            ui.label(t!(self.language, "Фильтр:", "Filter:"));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.model_filter)
                                    .desired_width(240.0)
                                    .hint_text(t!(self.language, "Название или ID", "Name or ID")),
                            );
                        });
                        let selected = self
                            .models
                            .iter()
                            .find(|m| m.id == self.model_id)
                            .map(|m| format!("{} — {}", m.name, m.id))
                            .unwrap_or_else(|| self.model_id.clone());
                        let filter = self.model_filter.to_lowercase();
                        egui::ComboBox::from_id_salt("openrouter-model")
                            .selected_text(selected)
                            .width(640.0)
                            .height(280.0)
                            .show_ui(ui, |ui| {
                                for model in &self.models {
                                    if filter.is_empty()
                                        || model.id.to_lowercase().contains(&filter)
                                        || model.name.to_lowercase().contains(&filter)
                                    {
                                        ui.selectable_value(
                                            &mut self.model_id,
                                            model.id.clone(),
                                            format!("{} — {}", model.name, model.id),
                                        );
                                    }
                                }
                            });
                    }
                    ui.label(
                        RichText::new(if self.catalog_loaded {
                            t!(
                                self.language,
                                "Каталог загружен по вашему запросу. Доступность и цена модели всё равно проверяются OpenRouter во время отправки.",
                                "The catalog was loaded on request. Availability and pricing are still checked by OpenRouter when the request is sent."
                            )
                        } else {
                            t!(
                                self.language,
                                "Текущий список — это офлайн-примеры. Он не гарантирует доступность модели. Кнопка каталога отправляет только публичный GET без ключа и без отчёта.",
                                "The current list contains offline examples. It does not guarantee availability. The catalog button sends only a public GET request with no key and no report."
                            )
                        })
                        .small()
                        .weak(),
                    );
                });

                card(ui, "🛡", t!(self.language, "Приватность и параметры запроса", "Privacy and request parameters"), |ui| {
                    ui.checkbox(
                        &mut self.privacy.hide_identifiers,
                        t!(
                            self.language,
                            "Скрывать hostname, MAC, серийные номера, имена интерфейсов и пути томов",
                            "Hide hostname, MAC addresses, serial numbers, interface names, and volume paths"
                        ),
                    );
                    ui.checkbox(
                        &mut self.privacy.include_processes,
                        t!(
                            self.language,
                            "Включить топ-10 процессов (названия, PID, память, CPU)",
                            "Include the top 10 processes (names, PID, memory, CPU)"
                        ),
                    );
                    ui.label(
                        RichText::new(t!(
                            self.language,
                            "Скрытие идентификаторов не гарантирует анонимность: модели устройств остаются. Перед отправкой вы увидите точный JSON.",
                            "Hiding identifiers does not guarantee anonymity because device models remain. Before sending, you will see the exact JSON."
                        ))
                        .small()
                        .weak(),
                    );
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(t!(self.language, "Тайм-аут, секунд:", "Timeout, seconds:"));
                        ui.add(egui::DragValue::new(&mut self.timeout_seconds).range(30..=600));
                        ui.label(t!(self.language, "Лимит токенов:", "Token limit:"));
                        ui.add(
                            egui::DragValue::new(&mut self.max_tokens)
                                .range(1024..=8192)
                                .speed(128.0),
                        );
                    });
                    ui.checkbox(
                        &mut self.deny_data_collection,
                        t!(
                            self.language,
                            "Только провайдеры с data_collection=deny",
                            "Only providers with data_collection=deny"
                        ),
                    );
                    ui.label(
                        RichText::new(t!(
                            self.language,
                            "Этот фильтр может сделать модель недоступной. Он не гарантирует нулевое хранение данных всеми участниками — проверьте политику OpenRouter и провайдера.",
                            "This filter can make a model unavailable. It does not guarantee zero data retention by every participant — check the OpenRouter and provider privacy policies."
                        ))
                        .small()
                        .weak(),
                    );
                });

                card(ui, "🚀", t!(self.language, "Запуск анализа", "Run analysis"), |ui| {
                    let target = self.analysis_job_language
                        .unwrap_or_else(|| ai::AnalysisLanguage::from(self.language));
                    ui.label(RichText::new(format!("{}: {}",
                        t!(self.language, "Язык запроса", "Requested response language"),
                        target.label(),
                    )).strong());
                    if self.analysis_job.is_some() && target != ai::AnalysisLanguage::from(self.language) {
                        ui.label(t!(self.language,
                            "Язык интерфейса изменён после отправки. Текущий запрос использует язык, выбранный при подтверждении; следующий запрос будет на новом языке.",
                            "The interface language changed after sending. This request keeps its confirmed language; the next request will use the new language."
                        ));
                    }
                    ui.horizontal_wrapped(|ui| {
                        let ready = self.snapshot.is_some() && self.analysis_job.is_none();
                        if ui
                            .add_enabled(
                                ready,
                                egui::Button::new(RichText::new(t!(self.language, "Проанализировать через ИИ", "Analyze with AI")).strong()),
                            )
                            .clicked()
                        {
                            match self.prepare_send() {
                                Ok(pending) => self.dialog = Some(Dialog::Send(pending)),
                                Err(error) => self.error(error),
                            }
                        }
                        if let Some(job) = &self.analysis_job {
                            ui.spinner();
                            ui.label(if matches!(self.language, Language::Ru) {
                                format!("Ожидание ответа · {} с", job.started.elapsed().as_secs())
                            } else {
                                format!("Waiting for response · {} s", job.started.elapsed().as_secs())
                            });
                        } else if self.snapshot.is_none() {
                            ui.label(t!(self.language, "Сначала дождитесь локального отчёта.", "Wait for the local report first."));
                        } else {
                            ui.label(
                                RichText::new(t!(
                                    self.language,
                                    "Сначала откроется окно подтверждения. Запрос может быть платным.",
                                    "A confirmation window will open first. The request may be billable."
                                ))
                                .small()
                                .weak(),
                            );
                        }
                    });
                    if self.analysis_job.is_some() {
                        ui.label(
                            RichText::new(t!(
                                self.language,
                                "Интерфейс остаётся доступным. Автоматических повторов нет; закрытие окна не гарантирует отмену оплаты уже принятого запроса.",
                                "The interface remains responsive. There are no automatic retries; closing the window does not guarantee cancellation of a request that may already have been accepted and billed."
                            ))
                            .small()
                            .weak(),
                        );
                    }
                });

                card(ui, "📝", t!(self.language, "Результат анализа", "Analysis result"), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(
                                !self.analysis.is_empty(),
                                egui::Button::new(t!(self.language, "⧉ Скопировать анализ", "⧉ Copy analysis")),
                            )
                            .clicked()
                        {
                            ctx.copy_text(self.analysis.clone());
                            self.notice = Some(
                                t!(
                                    self.language,
                                    "Анализ скопирован в буфер обмена.",
                                    "The analysis has been copied to the clipboard."
                                )
                                .to_owned(),
                            );
                        }
                        if ui
                            .add_enabled(
                                !self.analysis.is_empty() && self.save_job.is_none(),
                                egui::Button::new(t!(self.language, "💾 Сохранить ai_analysis.md", "💾 Save ai_analysis.md")),
                            )
                            .clicked()
                        {
                            self.dialog = Some(Dialog::Save(PendingSave {
                                title: t!(self.language, "Сохранить ИИ-анализ", "Save AI analysis").to_owned(),
                                contents: self.analysis.clone(),
                                path: default_save_path("ai_analysis.md"),
                                contains_identifiers: false,
                            }));
                        }
                    });

                    if let Some(metadata) = &self.analysis_metadata {
                        ui.label(RichText::new(format!("{}: {}",
                            t!(self.language, "Язык этого запроса", "Language requested for this result"),
                            ai::AnalysisLanguage::from(metadata.language).label(),
                        )).small());
                        if metadata.language != self.language {
                            ui.colored_label(Color32::from_rgb(214, 138, 58), t!(self.language,
                                "Этот результат запрошен на другом языке. Переключение RU/EN не переводит готовый анализ и не удаляет ваши правки. Для ответа на текущем языке запустите новый анализ с подтверждением.",
                                "This result was requested in a different language. Switching RU/EN does not translate the existing analysis or remove your edits. Start and confirm a new analysis for the current language."
                            ));
                        }
                        if metadata.generation != self.generation {
                            ui.colored_label(
                                Color32::from_rgb(214, 138, 58),
                                if matches!(self.language, Language::Ru) {
                                    format!(
                                        "Этот анализ относится к снимку №{}, а текущий отчёт — №{}. Для нового снимка нужен отдельный подтверждённый запрос.",
                                        metadata.generation, self.generation
                                    )
                                } else {
                                    format!(
                                        "This analysis belongs to snapshot #{}, while the current report is #{}. A new snapshot requires a separate confirmed request.",
                                        metadata.generation, self.generation
                                    )
                                },
                            );
                        }
                        for notice in &metadata.result.notices {
                            ui.colored_label(Color32::from_rgb(214, 138, 58), notice);
                        }
                    }

                    if self.analysis.is_empty() {
                        ui.label(
                            RichText::new(t!(
                                self.language,
                                "Здесь появится текстовое объяснение конфигурации. Это рекомендации модели, а не аппаратная диагностика или бенчмарк.",
                                "A text explanation of the configuration will appear here. It is model guidance, not a hardware diagnostic or benchmark."
                            ))
                            .weak(),
                        );
                    } else {
                        ui.label(
                            RichText::new(t!(
                                self.language,
                                "Markdown можно редактировать перед сохранением. Предыдущий результат остаётся здесь до успешного нового ответа.",
                                "You can edit the Markdown before saving it. The previous result remains here until a new request succeeds."
                            ))
                            .small()
                            .weak(),
                        );
                        ui.add(
                            egui::TextEdit::multiline(&mut self.analysis)
                                .id_salt("analysis-text")
                                .desired_width(f32::INFINITY)
                                .desired_rows(22),
                        );
                    }
                });
            });
    }

    fn prepare_send(&self) -> Result<PendingSend> {
        let language = ai::AnalysisLanguage::from(self.language);
        ai::validate_key(&self.api_key, language)?;
        let snapshot = self.snapshot.as_ref().context(t!(
            self.language,
            "Системный отчёт ещё не собран",
            "The system report is not available yet"
        ))?;
        let request = prepare_analysis_request(
            &snapshot.report,
            self.privacy,
            &self.model_id,
            self.language,
            self.timeout_seconds,
            self.max_tokens,
            self.deny_data_collection,
        )?;
        Ok(PendingSend {
            request,
            generation: self.generation,
            timestamp: snapshot.report.collected_at_unix_seconds,
            privacy: self.privacy,
            confirmed: false,
        })
    }

    fn dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;

        match &mut dialog {
            Dialog::Send(pending) => {
                let lang = Language::from(pending.request.language);
                egui::Window::new(t!(lang, "Подтверждение отправки данных", "Confirm data upload"))
                    .id(egui::Id::new("send-confirmation"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .default_width(820.0)
                    .max_width(980.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("send-dialog-scroll")
                            .max_height(620.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new(t!(lang, "JSON будет передан OpenRouter и выбранному провайдеру модели.", "The JSON will be sent to OpenRouter and the selected model provider.")).strong());
                                ui.label(format!("URL: {}", ai::API_URL));
                                ui.label(format!("{}: {}", t!(lang, "Модель", "Model"), pending.request.model));
                                ui.label(RichText::new(format!("{}: {}",
                                    t!(lang, "Язык запроса", "Requested response language"),
                                    pending.request.language.label(),
                                )).strong());
                                ui.label(if matches!(lang, Language::Ru) {
                                    format!(
                                        "Снимок №{} • Unix UTC {} • JSON {} байт",
                                        pending.generation, pending.timestamp, pending.request.report_json.len()
                                    )
                                } else {
                                    format!(
                                        "Snapshot #{} • Unix UTC {} • JSON {} bytes",
                                        pending.generation, pending.timestamp, pending.request.report_json.len()
                                    )
                                });
                                ui.label(if matches!(lang, Language::Ru) {
                                    format!(
                                        "Тайм-аут: {} с • токены: {} • data_collection=deny: {}",
                                        pending.request.timeout_seconds,
                                        pending.request.max_tokens,
                                        yes_no(pending.request.deny_data_collection, lang)
                                    )
                                } else {
                                    format!(
                                        "Timeout: {} s • tokens: {} • data_collection=deny: {}",
                                        pending.request.timeout_seconds,
                                        pending.request.max_tokens,
                                        yes_no(pending.request.deny_data_collection, lang)
                                    )
                                });
                                ui.colored_label(
                                    Color32::from_rgb(214, 138, 58),
                                    t!(lang, "Запрос может списать средства с баланса. Это внешняя передача данных, не локальный ИИ.", "The request may consume credits. This is an external data transfer, not a local AI model."),
                                );
                                if pending.generation != self.generation {
                                    ui.label(t!(lang, "Отчёт уже обновился, но будет отправлен именно этот показанный снимок. Отмените окно, чтобы выбрать новый.", "The report has already been refreshed, but this exact snapshot will be sent. Cancel the dialog if you want to choose the new one."));
                                }
                                egui::CollapsingHeader::new(t!(lang, "Системная инструкция модели", "System prompt"))
                                    .show(ui, |ui| {
                                        let mut prompt = pending.request.language.system_prompt();
                                        ui.add(
                                            egui::TextEdit::multiline(&mut prompt)
                                                .desired_width(f32::INFINITY)
                                                .desired_rows(14),
                                        );
                                    });
                                egui::CollapsingHeader::new(t!(lang, "Сообщение пользователя для API", "API user message"))
                                    .show(ui, |ui| {
                                        let message = pending.request.user_message();
                                        let mut text = message.as_str();
                                        ui.add(egui::TextEdit::multiline(&mut text)
                                            .desired_width(f32::INFINITY).desired_rows(8));
                                    });
                                ui.label(RichText::new(t!(lang, "Точный JSON для отправки", "Exact JSON to be sent")).strong());
                                egui::ScrollArea::both()
                                    .id_salt("send-json-scroll")
                                    .max_height(260.0)
                                    .show(ui, |ui| selectable_json(ui, "send-json", &pending.request.report_json));
                                ui.checkbox(
                                    &mut pending.confirmed,
                                    t!(lang, "Я просмотрел(а) данные и подтверждаю их передачу", "I reviewed the data and confirm that it may be uploaded"),
                                );
                                ui.horizontal(|ui| {
                                    submit = ui
                                        .add_enabled(
                                            pending.confirmed,
                                            egui::Button::new(t!(lang, "Отправить", "Send")),
                                        )
                                        .clicked();
                                    cancel = ui.button(t!(lang, "Отмена", "Cancel")).clicked();
                                });
                            });
                    });
            }
            Dialog::Save(pending) => {
                let lang = self.language;
                egui::Window::new(&pending.title)
                    .id(egui::Id::new("save-confirmation"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .default_width(760.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(t!(lang, "Полный путь к файлу (папка должна существовать):", "Full path to the file (the directory must already exist):"));
                        ui.add(egui::TextEdit::singleline(&mut pending.path).desired_width(f32::INFINITY));
                        ui.label(t!(lang, "По умолчанию — рядом с .exe. Можно указать другую папку, доступную для записи.", "By default the file is saved next to the .exe. You can choose another writable directory."));
                        ui.colored_label(Color32::from_rgb(214, 138, 58), t!(lang, "Существующий файл с этим именем будет заменён после нажатия «Сохранить».", "An existing file with the same name will be replaced after you click Save."));
                        if pending.contains_identifiers {
                            ui.label(t!(lang, "Это полный локальный отчёт: в нём могут быть hostname, MAC, серийные номера и названия процессов.", "This is the full local report: it may contain the hostname, MAC addresses, serial numbers, and process names."));
                        } else {
                            ui.label(t!(lang, "Будет сохранён текущий текст анализа, включая ваши правки. Он тоже может содержать сведения о компьютере.", "The current analysis text will be saved together with your edits. It may also contain information about the computer."));
                        }
                        ui.horizontal(|ui| {
                            submit = ui.button(t!(lang, "Сохранить", "Save")).clicked();
                            cancel = ui.button(t!(lang, "Отмена", "Cancel")).clicked();
                        });
                    });
            }
        }

        if !open || cancel {
            return;
        }
        if !submit {
            self.dialog = Some(dialog);
            return;
        }

        match dialog {
            Dialog::Send(pending) => {
                if self.analysis_job.is_some() || !pending.confirmed {
                    return;
                }
                let key = std::mem::take(&mut self.api_key);
                self.show_key = false;
                let request_language = pending.request.language;
                let job = spawn_job("openrouter-analysis", ctx, move || {
                    let result = ai::analyze(key, pending.request)?;
                    Ok(CompletedAnalysis {
                        result,
                        generation: pending.generation,
                        timestamp: pending.timestamp,
                        privacy: pending.privacy,
                        language: Language::from(request_language),
                    })
                });
                match job {
                    Ok(job) => {
                        self.analysis_job = Some(job);
                        self.analysis_job_language = Some(request_language);
                    }
                    Err(error) => self.error(error),
                }
            }
            Dialog::Save(pending) => {
                let path = PathBuf::from(pending.path.trim());
                if !path.is_absolute() || path.file_name().is_none() {
                    self.error(anyhow!(t!(
                        self.language,
                        "Укажите абсолютный путь с именем файла, например C:\\Users\\Name\\Documents\\system_report.json.",
                        "Specify an absolute path with a file name, for example C:\\Users\\Name\\Documents\\system_report.json."
                    )));
                    self.dialog = Some(Dialog::Save(pending));
                    return;
                }
                match spawn_job("save-file", ctx, move || {
                    atomic_save(&path, pending.contents.as_bytes())?;
                    Ok(path)
                }) {
                    Ok(job) => self.save_job = Some(job),
                    Err(error) => self.error(error),
                }
            }
        }
    }
}

/// Build and localize the privacy-filtered snapshot before it is shown for consent.
/// The language belongs to this immutable request, not to the mutable GUI state.
fn prepare_analysis_request(
    report: &SystemReport,
    privacy: PrivacyOptions,
    model: &str,
    language: Language,
    timeout_seconds: u64,
    max_tokens: u32,
    deny_data_collection: bool,
) -> Result<ai::AnalysisRequest> {
    let language = ai::AnalysisLanguage::from(language);
    let private_json = report.json_for_ai(privacy)?;
    let request = ai::AnalysisRequest {
        model: model.trim().to_owned(),
        report_json: ai::report_json_for_language(&private_json, language)?,
        timeout_seconds,
        max_tokens,
        deny_data_collection,
        language,
    };
    request.validate()?;
    Ok(request)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        let background_enabled = self.dialog.is_none();

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_enabled_ui(background_enabled, |ui| self.header(ui, ctx));
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_enabled_ui(background_enabled, |ui| {
                if !self.errors.is_empty() {
                    card(ui, "⛔", t!(self.language, "Ошибки", "Errors"), |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                Color32::from_rgb(216, 86, 86),
                                t!(self.language, "Операция завершилась с ошибкой", "An operation failed"),
                            );
                            if ui.small_button(t!(self.language, "Закрыть", "Dismiss")).clicked() {
                                self.errors.clear();
                            }
                        });
                        egui::ScrollArea::vertical()
                            .id_salt("error-scroll")
                            .max_height(110.0)
                            .show(ui, |ui| {
                                for error in &self.errors {
                                    ui.label(RichText::new(error).color(Color32::from_rgb(216, 86, 86)));
                                }
                            });
                    });
                }
                if let Some(notice) = &self.notice {
                    ui.label(RichText::new(notice).small());
                }
                if self.save_job.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t!(self.language, "Сохранение файла…", "Saving file…"));
                    });
                }
                ui.label(
                    RichText::new(t!(
                        self.language,
                        "SysInfo AI 0.3 · AI-L1 · без фоновой телеметрии · ключ и настройки не сохраняются",
                        "SysInfo AI 0.3 · AI-L1 · no background telemetry · key and settings are not persisted"
                    ))
                    .small()
                    .weak(),
                );
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_enabled_ui(background_enabled, |ui| match self.tab {
                Tab::System => self.system_ui(ui, ctx),
                Tab::Ai => self.ai_ui(ui, ctx),
            });
        });

        self.dialog_ui(ctx);
        if self.collect_job.is_some()
            || self.analysis_job.is_some()
            || self.models_job.is_some()
            || self.save_job.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

fn apply_text_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.indent = 18.0;
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(24.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(15.5));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(13.5));
    ctx.set_style(style);
}

fn apply_theme(ctx: &egui::Context, theme: ThemeMode) {
    let mut visuals = match theme {
        ThemeMode::Light => egui::Visuals::light(),
        ThemeMode::Dark => egui::Visuals::dark(),
    };
    match theme {
        ThemeMode::Light => {
            visuals.panel_fill = Color32::from_rgb(247, 249, 252);
            visuals.extreme_bg_color = Color32::from_rgb(255, 255, 255);
            visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(255, 255, 255);
            visuals.widgets.inactive.bg_fill = Color32::from_rgb(250, 252, 255);
            visuals.widgets.hovered.bg_fill = Color32::from_rgb(238, 244, 255);
            visuals.widgets.active.bg_fill = Color32::from_rgb(225, 236, 255);
            visuals.widgets.open.bg_fill = Color32::from_rgb(243, 248, 255);
            visuals.selection.bg_fill = Color32::from_rgb(92, 135, 245);
            visuals.hyperlink_color = Color32::from_rgb(61, 104, 224);
            visuals.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(212, 219, 229);
            visuals.window_shadow.color = Color32::from_black_alpha(18);
        }
        ThemeMode::Dark => {
            visuals.selection.bg_fill = Color32::from_rgb(92, 135, 245);
            visuals.hyperlink_color = Color32::from_rgb(118, 171, 255);
        }
    }
    ctx.set_visuals(visuals);
}

fn render_summary_cards(ui: &mut egui::Ui, language: Language, report: &SystemReport) {
    let physical = report
        .cpu
        .physical_cores
        .map(|v| v.to_string())
        .unwrap_or_else(|| "—".to_owned());
    let gpu_name = report
        .gpu
        .iter()
        .find_map(|gpu| gpu.name.as_deref())
        .or_else(|| {
            report
                .dxgi_adapters
                .first()
                .map(|adapter| adapter.name.as_str())
        })
        .unwrap_or("—");
    let metrics = [
        SummaryMetric {
            title: t!(language, "Процессор", "Processor"),
            value: report.cpu.brand.as_deref().unwrap_or("—").to_owned(),
            subtitle: if matches!(language, Language::Ru) {
                format!("{} физ. · {} логич.", physical, report.cpu.logical_cores)
            } else {
                format!(
                    "{} physical · {} logical",
                    physical, report.cpu.logical_cores
                )
            },
        },
        SummaryMetric {
            title: "RAM",
            value: fmt_size(report.memory.total),
            subtitle: format!(
                "{} {}",
                t!(language, "Использовано", "Used"),
                fmt_size(report.memory.used)
            ),
        },
        SummaryMetric {
            title: "GPU",
            value: gpu_name.to_owned(),
            subtitle: format!(
                "{} {}",
                t!(language, "Адаптеров", "Adapters"),
                report.dxgi_adapters.len().max(report.gpu.len())
            ),
        },
        SummaryMetric {
            title: t!(language, "ОС", "OS"),
            value: report
                .os
                .long_version
                .as_deref()
                .or(report.os.version.as_deref())
                .unwrap_or("—")
                .to_owned(),
            subtitle: fmt_uptime(report.os.uptime_seconds, language),
        },
    ];
    let columns = summary_columns(ui.available_width(), ui.spacing().item_spacing.x);
    for (row_index, row) in metrics.chunks(columns).enumerate() {
        ui.push_id(("summary-row", row_index), |ui| {
            summary_row(ui, row);
        });
    }
}

fn section_selector(ui: &mut egui::Ui, language: Language, section: &mut SystemSection) {
    card(
        ui,
        "🗂",
        t!(language, "Разделы оборудования", "Hardware sections"),
        |ui| {
            ui.horizontal_wrapped(|ui| {
                section_tab(
                    ui,
                    section,
                    SystemSection::Overview,
                    "🏠",
                    t!(language, "Обзор", "Overview"),
                );
                section_tab(ui, section, SystemSection::Os, "🪟", "OS");
                section_tab(ui, section, SystemSection::Cpu, "🧠", "CPU");
                section_tab(
                    ui,
                    section,
                    SystemSection::Memory,
                    "💾",
                    t!(language, "Память", "Memory"),
                );
                section_tab(
                    ui,
                    section,
                    SystemSection::Storage,
                    "🗄",
                    t!(language, "Диски", "Storage"),
                );
                section_tab(ui, section, SystemSection::Gpu, "🎮", "GPU");
                section_tab(
                    ui,
                    section,
                    SystemSection::Board,
                    "🧱",
                    t!(language, "Плата / BIOS", "Board / BIOS"),
                );
                section_tab(
                    ui,
                    section,
                    SystemSection::Network,
                    "🌐",
                    t!(language, "Сеть", "Network"),
                );
                section_tab(
                    ui,
                    section,
                    SystemSection::Processes,
                    "📈",
                    t!(language, "Процессы", "Processes"),
                );
                section_tab(ui, section, SystemSection::Json, "{ }", "JSON");
            });
        },
    );
}

fn tab_button<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    selected: T,
    _icon: &str,
    label: &str,
) {
    if ui.selectable_label(*value == selected, label).clicked() {
        *value = selected;
    }
}

fn section_tab(
    ui: &mut egui::Ui,
    value: &mut SystemSection,
    selected: SystemSection,
    icon: &str,
    label: &str,
) {
    tab_button(ui, value, selected, icon, label);
}

/// Set the *content* width, subtracting padding and border exactly once.
/// Setting only a child Ui's allocation does not stretch a Frame's painted rect.
fn full_width_frame<R>(
    ui: &mut egui::Ui,
    frame: egui::Frame,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let width = content_width(ui.available_width(), frame.total_margin().sum().x);
    frame.show(ui, |ui| {
        ui.set_width(width);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        add(ui)
    })
}

fn content_width(outer_width: f32, horizontal_margin: f32) -> f32 {
    (outer_width - horizontal_margin).max(0.0)
}

fn card_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::group(ui.style())
        .fill(panel_fill(ui))
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .inner_margin(egui::Margin::same(12))
}

fn card<R>(ui: &mut egui::Ui, _icon: &str, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let frame = card_frame(ui);
    full_width_frame(ui, frame, |ui| {
        ui.add(egui::Label::new(RichText::new(title).strong().size(17.0)).wrap());
        ui.add_space(6.0);
        add(ui)
    })
    .inner
}

fn subcard<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let frame = egui::Frame::group(ui.style())
        .fill(ui.visuals().widgets.inactive.bg_fill)
        .inner_margin(egui::Margin::same(10));
    full_width_frame(ui, frame, |ui| {
        ui.add(egui::Label::new(RichText::new(title).strong()).wrap());
        ui.add_space(4.0);
        add(ui)
    })
    .inner
}

/// Breakpoints affect the number of columns, never the maximum window width.
fn summary_columns(width: f32, gap: f32) -> usize {
    const MIN_CARD_WIDTH: f32 = 240.0;
    if width >= MIN_CARD_WIDTH * 4.0 + gap * 3.0 {
        4
    } else if width >= MIN_CARD_WIDTH * 2.0 + gap {
        2
    } else {
        1
    }
}

struct SummaryMetric {
    title: &'static str,
    value: String,
    subtitle: String,
}

/// Paint equally wide and equally tall cards after measuring all content in a row.
/// The tallest wrapped text determines row height: no clipping or height ceiling.
fn summary_row(ui: &mut egui::Ui, metrics: &[SummaryMetric]) {
    if metrics.is_empty() {
        return;
    }
    ui.columns(metrics.len(), |columns| {
        let mut frames = Vec::with_capacity(metrics.len());
        for (column, metric) in columns.iter_mut().zip(metrics) {
            let frame = card_frame(column);
            let width = content_width(column.available_width(), frame.total_margin().sum().x);
            let mut prepared = frame.begin(column);
            let content = &mut prepared.content_ui;
            content.set_width(width);
            content.add(egui::Label::new(RichText::new(metric.title).small().strong()).wrap());
            content.add(egui::Label::new(RichText::new(&metric.value).size(17.0)).wrap());
            content.add(egui::Label::new(RichText::new(&metric.subtitle).small().weak()).wrap());
            frames.push(prepared);
        }
        let row_height = frames
            .iter()
            .map(|frame| frame.content_ui.min_size().y)
            .fold(0.0_f32, f32::max);
        for (mut frame, column) in frames.into_iter().zip(columns.iter_mut()) {
            frame.content_ui.set_min_height(row_height);
            let _ = frame.end(column);
        }
    });
}

/// Use a second column only when both cards can retain readable field widths.
/// Each final row also occupies the full width, including an odd last item.
fn responsive_cards(
    ui: &mut egui::Ui,
    id: &str,
    count: usize,
    mut add: impl FnMut(&mut egui::Ui, usize),
) {
    let columns = if ui.available_width() >= 840.0 + ui.spacing().item_spacing.x {
        2
    } else {
        1
    };
    ui.push_id(id, |ui| {
        for start in (0..count).step_by(columns) {
            let row_count = columns.min(count - start);
            ui.push_id(("row", start), |ui| {
                ui.columns(row_count, |row| {
                    for (offset, column) in row.iter_mut().enumerate() {
                        let index = start + offset;
                        column.push_id(index, |ui| add(ui, index));
                    }
                });
            });
        }
    });
}

fn badge(ui: &mut egui::Ui, color: Color32, text: impl Into<String>) {
    egui::Frame::default()
        .fill(color.linear_multiply(0.12))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.7)))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text.into()).small().color(color));
        });
}

fn empty_state(ui: &mut egui::Ui, icon: &str, title: &str, text: &str) {
    ui.add_space(24.0);
    card(ui, icon, title, |ui| {
        ui.label(text);
    });
}

fn key_value(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(format!("{key}: ")).strong());
        ui.label(value);
    });
}

fn key_value_row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.label(RichText::new(key).strong());
    ui.label(value);
    ui.end_row();
}

fn header_cell(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).strong());
}

fn data_grid<R>(
    ui: &mut egui::Ui,
    id: &str,
    columns: usize,
    spacing: [f32; 2],
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let width =
        (ui.available_width() - spacing[0] * (columns - 1) as f32).max(0.0) / columns as f32;
    egui::Grid::new(id)
        .num_columns(columns)
        .spacing(spacing)
        .min_col_width(width)
        .max_col_width(width)
        .striped(true)
        .show(ui, add)
        .inner
}

fn two_column_grid<R>(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    data_grid(ui, id, 2, [18.0, 8.0], add)
}

fn panel_fill(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        ui.visuals().widgets.noninteractive.bg_fill
    } else {
        Color32::from_rgb(255, 255, 255)
    }
}

fn render_cpu_package(ui: &mut egui::Ui, language: Language, package: &CpuPackageInfo) {
    two_column_grid(ui, "cpu-package-grid", |ui| {
        key_value_row(
            ui,
            t!(language, "Имя", "Name"),
            package.name.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Производитель", "Manufacturer"),
            package.manufacturer.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Сокет", "Socket"),
            package.socket.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Физические ядра", "Physical cores"),
            &package
                .physical_cores
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        key_value_row(
            ui,
            t!(language, "Логические ядра", "Logical cores"),
            &package
                .logical_cores
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        key_value_row(
            ui,
            t!(language, "Текущая частота", "Current clock"),
            &opt_u64(package.current_clock_mhz_reported, "MHz"),
        );
        key_value_row(
            ui,
            t!(language, "Макс. частота", "Max clock"),
            &opt_u64(package.max_clock_mhz_reported, "MHz"),
        );
        key_value_row(ui, "L2", &opt_u64(package.l2_cache_kib, "KiB"));
        key_value_row(ui, "L3", &opt_u64(package.l3_cache_kib, "KiB"));
    });
}

fn render_memory_module(ui: &mut egui::Ui, language: Language, module: &MemoryModule) {
    two_column_grid(ui, "memory-module-grid", |ui| {
        key_value_row(
            ui,
            t!(language, "Слот", "Slot"),
            module.slot.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Банк", "Bank"),
            module.bank.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Производитель", "Manufacturer"),
            module.manufacturer.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Part number", "Part number"),
            module.part_number.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Serial", "Serial"),
            module.serial_number.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Объём", "Capacity"),
            &module.capacity.map(fmt_size).unwrap_or_else(|| "—".into()),
        );
        key_value_row(
            ui,
            t!(language, "Скорость", "Speed"),
            &opt_u64(module.speed_mhz_reported, "MHz"),
        );
        key_value_row(
            ui,
            t!(language, "Configured clock", "Configured clock"),
            &opt_u64(module.configured_clock_speed_mhz_reported, "MHz"),
        );
        key_value_row(
            ui,
            t!(language, "SMBIOS type", "SMBIOS type"),
            &module
                .smbios_memory_type_code
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        key_value_row(
            ui,
            t!(language, "Form factor", "Form factor"),
            &module
                .form_factor_code
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        key_value_row(
            ui,
            t!(language, "Data width", "Data width"),
            &module
                .data_width_bits
                .map(|v| format!("{v} bit"))
                .unwrap_or_else(|| "—".into()),
        );
        key_value_row(
            ui,
            t!(language, "Total width", "Total width"),
            &module
                .total_width_bits
                .map(|v| format!("{v} bit"))
                .unwrap_or_else(|| "—".into()),
        );
    });
}

fn render_volume(ui: &mut egui::Ui, language: Language, disk: &DiskInfo) {
    two_column_grid(ui, "volume-grid", |ui| {
        key_value_row(ui, t!(language, "Имя", "Name"), &disk.name);
        key_value_row(
            ui,
            t!(language, "Точка монтирования", "Mount point"),
            &disk.mount_point,
        );
        key_value_row(ui, t!(language, "Всего", "Total"), &fmt_size(disk.total));
        key_value_row(ui, t!(language, "Свободно", "Free"), &fmt_size(disk.free));
        key_value_row(ui, t!(language, "Занято", "Used"), &fmt_size(disk.used));
        key_value_row(
            ui,
            t!(language, "Файловая система", "File system"),
            &disk.file_system,
        );
        key_value_row(ui, t!(language, "Тип", "Kind"), &disk.kind_reported);
        key_value_row(
            ui,
            t!(language, "Съёмный", "Removable"),
            if disk.is_removable {
                t!(language, "Да", "Yes")
            } else {
                t!(language, "Нет", "No")
            },
        );
    });
}

fn render_physical_disk(ui: &mut egui::Ui, language: Language, disk: &PhysicalDiskInfo) {
    two_column_grid(ui, "physical-disk-grid", |ui| {
        key_value_row(
            ui,
            t!(language, "Модель", "Model"),
            disk.model.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Производитель", "Manufacturer"),
            disk.manufacturer.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Серийный номер", "Serial number"),
            disk.serial_number.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Прошивка", "Firmware"),
            disk.firmware_revision.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Интерфейс", "Interface"),
            disk.interface_type_reported.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Носитель", "Media type"),
            disk.media_type_reported.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Размер", "Size"),
            &disk.size.map(fmt_size).unwrap_or_else(|| "—".into()),
        );
        key_value_row(
            ui,
            t!(language, "Статус", "Status"),
            disk.status_reported.as_deref().unwrap_or("—"),
        );
    });
}

fn render_gpu(ui: &mut egui::Ui, language: Language, gpu: &GpuInfo) {
    two_column_grid(ui, "gpu-grid", |ui| {
        key_value_row(
            ui,
            t!(language, "Название", "Name"),
            gpu.name.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Производитель", "Manufacturer"),
            gpu.manufacturer.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Видеочип", "Video processor"),
            gpu.video_processor.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Драйвер", "Driver version"),
            gpu.driver_version.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Дата драйвера", "Driver date"),
            gpu.driver_date_wmi.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "AdapterRAM (WMI)", "AdapterRAM (WMI)"),
            &gpu.adapter_ram_wmi_reported
                .map(fmt_size)
                .unwrap_or_else(|| "—".into()),
        );
        key_value_row(
            ui,
            t!(language, "AdapterRAM надёжен", "AdapterRAM reliable"),
            yes_no(gpu.adapter_ram_is_reliable, language),
        );
        key_value_row(
            ui,
            t!(language, "Разрешение", "Resolution"),
            &format!(
                "{} × {}",
                gpu.current_horizontal_resolution
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into()),
                gpu.current_vertical_resolution
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into())
            ),
        );
        key_value_row(
            ui,
            t!(language, "Частота экрана", "Refresh rate"),
            &opt_u64(gpu.current_refresh_rate_hz_reported, "Hz"),
        );
        key_value_row(
            ui,
            t!(language, "Статус", "Status"),
            gpu.status_reported.as_deref().unwrap_or("—"),
        );
    });
}

fn render_dxgi(ui: &mut egui::Ui, language: Language, adapter: &DxgiAdapterInfo) {
    two_column_grid(ui, "dxgi-grid", |ui| {
        key_value_row(ui, t!(language, "Название", "Name"), &adapter.name);
        key_value_row(
            ui,
            t!(language, "Vendor ID", "Vendor ID"),
            &format!("0x{:04X}", adapter.vendor_id),
        );
        key_value_row(
            ui,
            t!(language, "Device ID", "Device ID"),
            &format!("0x{:04X}", adapter.device_id),
        );
        key_value_row(
            ui,
            t!(language, "Dedicated VRAM", "Dedicated VRAM"),
            &fmt_size(adapter.dedicated_video_memory),
        );
        key_value_row(
            ui,
            t!(language, "Dedicated system", "Dedicated system"),
            &fmt_size(adapter.dedicated_system_memory),
        );
        key_value_row(
            ui,
            t!(language, "Shared system", "Shared system"),
            &fmt_size(adapter.shared_system_memory),
        );
        key_value_row(
            ui,
            t!(language, "Software adapter", "Software adapter"),
            yes_no(adapter.is_software_adapter, language),
        );
    });
}

fn render_board(ui: &mut egui::Ui, language: Language, board: &MotherboardInfo) {
    two_column_grid(ui, "board-grid", |ui| {
        key_value_row(
            ui,
            t!(language, "Производитель", "Manufacturer"),
            board.manufacturer.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Модель", "Product"),
            board.product.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Версия", "Version"),
            board.version.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Серийный номер", "Serial number"),
            board.serial_number.as_deref().unwrap_or("—"),
        );
    });
}

fn render_bios(ui: &mut egui::Ui, language: Language, bios: &BiosInfo) {
    two_column_grid(ui, "bios-grid", |ui| {
        key_value_row(
            ui,
            t!(language, "Производитель", "Manufacturer"),
            bios.manufacturer.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "SMBIOS версия", "SMBIOS version"),
            bios.smbios_bios_version.as_deref().unwrap_or("—"),
        );
        let version_strings = if bios.version_strings.is_empty() {
            "—".to_owned()
        } else {
            bios.version_strings.join(" / ")
        };
        key_value_row(
            ui,
            t!(language, "Версия", "Version strings"),
            &version_strings,
        );
        key_value_row(
            ui,
            t!(language, "Серийный номер", "Serial number"),
            bios.serial_number.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Дата релиза", "Release date"),
            bios.release_date_wmi.as_deref().unwrap_or("—"),
        );
    });
}

fn render_network(ui: &mut egui::Ui, language: Language, network: &NetworkInfo) {
    two_column_grid(ui, "network-grid", |ui| {
        key_value_row(
            ui,
            t!(language, "Интерфейс", "Interface"),
            &network.interface,
        );
        key_value_row(
            ui,
            t!(language, "MAC", "MAC"),
            network.mac_address.as_deref().unwrap_or("—"),
        );
        key_value_row(
            ui,
            t!(language, "Получено", "Received"),
            &format_bytes(network.total_received_bytes),
        );
        key_value_row(
            ui,
            t!(language, "Передано", "Transmitted"),
            &format_bytes(network.total_transmitted_bytes),
        );
        key_value_row(
            ui,
            t!(language, "Пакеты RX", "RX packets"),
            &network.total_received_packets.to_string(),
        );
        key_value_row(
            ui,
            t!(language, "Пакеты TX", "TX packets"),
            &network.total_transmitted_packets.to_string(),
        );
        key_value_row(
            ui,
            t!(language, "Ошибки RX", "RX errors"),
            &network.total_receive_errors.to_string(),
        );
        key_value_row(
            ui,
            t!(language, "Ошибки TX", "TX errors"),
            &network.total_transmit_errors.to_string(),
        );
    });
}

fn selectable_json(ui: &mut egui::Ui, id: &str, mut text: &str) {
    ui.add(
        egui::TextEdit::multiline(&mut text)
            .id_salt(id)
            .code_editor()
            .desired_width(f32::INFINITY)
            .desired_rows(12),
    );
}

fn yes_no(value: bool, language: Language) -> &'static str {
    if value {
        t!(language, "да", "yes")
    } else {
        t!(language, "нет", "no")
    }
}

fn format_analysis(completed: &CompletedAnalysis) -> String {
    let result = &completed.result;
    let language = completed.language;
    let mut text = if matches!(language, Language::Ru) {
        format!(
            "# Анализ конфигурации компьютера\n\nМодель: {}\n\nСнимок: №{}, время Unix UTC: {}.\n\nИдентификаторы скрыты: {}. Топ процессов включён: {}.\n\n",
            result.resolved_model,
            completed.generation,
            completed.timestamp,
            yes_no(completed.privacy.hide_identifiers, language),
            yes_no(completed.privacy.include_processes, language)
        )
    } else {
        format!(
            "# Computer configuration analysis\n\nModel: {}\n\nSnapshot: #{}, Unix UTC timestamp: {}.\n\nIdentifiers hidden: {}. Top processes included: {}.\n\n",
            result.resolved_model,
            completed.generation,
            completed.timestamp,
            yes_no(completed.privacy.hide_identifiers, language),
            yes_no(completed.privacy.include_processes, language)
        )
    };
    text.push_str(&format!(
        "{}: {}.\n\n",
        t!(language, "Язык запроса", "Requested response language"),
        ai::AnalysisLanguage::from(language).label(),
    ));
    if result.usage.prompt_tokens.is_some()
        || result.usage.completion_tokens.is_some()
        || result.usage.total_tokens.is_some()
    {
        let count = |v: Option<u64>| {
            v.map(|n| n.to_string()).unwrap_or_else(|| {
                if matches!(language, Language::Ru) {
                    "не сообщено".into()
                } else {
                    "not reported".into()
                }
            })
        };
        // Keep the owned String alive until push_str has borrowed it.
        let tokens_text = if matches!(language, Language::Ru) {
            format!(
                "Токены: вход — {}, выход — {}, всего — {}.\n\n",
                count(result.usage.prompt_tokens),
                count(result.usage.completion_tokens),
                count(result.usage.total_tokens)
            )
        } else {
            format!(
                "Tokens: input — {}, output — {}, total — {}.\n\n",
                count(result.usage.prompt_tokens),
                count(result.usage.completion_tokens),
                count(result.usage.total_tokens)
            )
        };
        text.push_str(&tokens_text);
    }
    if let Some(cost) = result.usage.cost_usd {
        text.push_str(&if matches!(language, Language::Ru) {
            format!("Стоимость по ответу API: ${cost:.6}.\n\n")
        } else {
            format!("API-reported cost: ${cost:.6}.\n\n")
        });
    }
    if let Some(id) = &result.generation_id {
        text.push_str(&if matches!(language, Language::Ru) {
            format!("ID запроса: {id}\n\n")
        } else {
            format!("Request ID: {id}\n\n")
        });
    }
    for notice in &result.notices {
        text.push_str(&format!("> {notice}\n\n"));
    }
    text.push_str("---\n\n");
    text.push_str(&result.text);
    text.push('\n');
    text
}

fn fmt_size(size: Size) -> String {
    format!("{:.2} GiB", size.gib)
}

fn opt_u64(value: Option<u64>, suffix: &str) -> String {
    match value {
        Some(v) if suffix.is_empty() => v.to_string(),
        Some(v) => format!("{v} {suffix}"),
        None => "—".into(),
    }
}

fn opt_f32_percent(value: Option<f32>) -> String {
    value
        .map(|v| format!("{v:.1}%"))
        .unwrap_or_else(|| "—".into())
}

fn opt_f64_percent(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.1}%"))
        .unwrap_or_else(|| "—".into())
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.2} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.2} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn fmt_uptime(seconds: u64, language: Language) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3600;
    let minutes = (seconds % 3600) / 60;
    if matches!(language, Language::Ru) {
        format!("{} д {} ч {} мин", days, hours, minutes)
    } else {
        format!("{} d {} h {} min", days, hours, minutes)
    }
}

fn default_save_path(filename: &str) -> String {
    let directory = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    directory.join(filename).to_string_lossy().into_owned()
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct TemporaryFile(PathBuf);
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn atomic_save(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("Не удалось определить папку назначения")?;
    let name = path
        .file_name()
        .context("В пути отсутствует имя файла")?
        .to_string_lossy();
    for _ in 0..64 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".{name}.{}.{sequence}.tmp", std::process::id()));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).with_context(|| {
                format!(
                    "Не удалось создать временный файл в {}. Выберите доступную для записи папку.",
                    parent.display()
                )
            }),
        };
        let guard = TemporaryFile(temporary.clone());
        let write_result = file.write_all(contents).and_then(|()| file.sync_all());
        drop(file);
        write_result.with_context(|| format!("Не удалось записать {}", path.display()))?;
        fs::rename(&temporary, path).with_context(|| {
            format!(
                "Не удалось заменить {}. Возможно, файл занят или нет прав записи.",
                path.display()
            )
        })?;
        drop(guard);
        return Ok(());
    }
    bail!("Не удалось подобрать свободное имя временного файла; целевой файл не изменён.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullwidth_summary_breakpoints() {
        assert_eq!(summary_columns(1920.0, 10.0), 4);
        assert_eq!(summary_columns(1200.0, 10.0), 4);
        assert_eq!(summary_columns(990.0, 10.0), 4);
        assert_eq!(summary_columns(989.0, 10.0), 2);
        assert_eq!(summary_columns(700.0, 10.0), 2);
        assert_eq!(summary_columns(490.0, 10.0), 2);
        assert_eq!(summary_columns(489.0, 10.0), 1);
        assert_eq!(summary_columns(320.0, 10.0), 1);
    }

    #[test]
    fn fullwidth_inner_size_accounts_for_padding_and_border() {
        assert_eq!(content_width(400.0, 26.0), 374.0);
        assert_eq!(content_width(12.0, 26.0), 0.0);
    }

    #[test]
    fn fullwidth_summary_uses_all_available_row_width() {
        for available in [
            320.0_f32, 490.0, 700.0, 989.0, 990.0, 1200.0, 1920.0, 3840.0,
        ] {
            let gap = 10.0;
            let n = summary_columns(available, gap);
            let card_width = (available - gap * (n - 1) as f32) / n as f32;
            let reconstructed = n as f32 * card_width + gap * (n - 1) as f32;
            assert!((reconstructed - available).abs() < 0.01);
        }
    }

    #[test]
    fn fullwidth_frame_geometry_and_natural_height() {
        let ctx = egui::Context::default();
        let mut requested_width = 0.0_f32;
        let mut rendered_width = 0.0_f32;
        let mut rendered_height = 0.0_f32;
        let mut viewport_height = 0.0_f32;
        // Two frames allow egui's first-frame font/layout bookkeeping to settle.
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 700.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    viewport_height = ui.available_height();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            requested_width = ui.available_width();
                            let frame = card_frame(ui);
                            let response = full_width_frame(ui, frame, |ui| {
                                for _ in 0..100 {
                                    ui.add(
                                        egui::Label::new("Unrestricted content / Полный текст")
                                            .wrap(),
                                    );
                                }
                            });
                            rendered_width = response.response.rect.width();
                            rendered_height = response.response.rect.height();
                        });
                });
            });
        }
        assert!((rendered_width - requested_width).abs() < 1.0);
        assert!(rendered_height > viewport_height);
    }

    fn analysis_fixture(language: Language, usage: ai::Usage) -> CompletedAnalysis {
        CompletedAnalysis {
            result: ai::AnalysisResult {
                text: "Example analysis.".to_owned(),
                resolved_model: "test/model".to_owned(),
                generation_id: None,
                usage,
                notices: Vec::new(),
            },
            generation: 7,
            timestamp: 1_750_000_000,
            privacy: PrivacyOptions::default(),
            language,
        }
    }

    #[test]
    fn analysis_token_summary_russian() {
        let completed = analysis_fixture(
            Language::Ru,
            ai::Usage {
                prompt_tokens: Some(10),
                completion_tokens: Some(20),
                total_tokens: Some(30),
                cost_usd: None,
            },
        );
        let text = format_analysis(&completed);
        assert!(text.contains("Токены: вход — 10, выход — 20, всего — 30.\n\n"));
        assert!(text.ends_with("Example analysis.\n"));
    }

    #[test]
    fn analysis_token_summary_english() {
        let completed = analysis_fixture(
            Language::En,
            ai::Usage {
                prompt_tokens: Some(10),
                completion_tokens: Some(20),
                total_tokens: Some(30),
                cost_usd: None,
            },
        );
        let text = format_analysis(&completed);
        assert!(text.contains("Tokens: input — 10, output — 20, total — 30.\n\n"));
        assert!(text.ends_with("Example analysis.\n"));
    }

    #[test]
    fn analysis_missing_token_counts_are_not_zero() {
        for (language, expected) in [
            (
                Language::Ru,
                "Токены: вход — не сообщено, выход — не сообщено, всего — 30.",
            ),
            (
                Language::En,
                "Tokens: input — not reported, output — not reported, total — 30.",
            ),
        ] {
            let completed = analysis_fixture(
                language,
                ai::Usage {
                    total_tokens: Some(30),
                    ..Default::default()
                },
            );
            assert!(format_analysis(&completed).contains(expected));
        }
    }

    #[test]
    fn analysis_omits_token_summary_when_usage_is_absent() {
        for language in [Language::Ru, Language::En] {
            let text = format_analysis(&analysis_fixture(language, ai::Usage::default()));
            assert!(!text.contains("Токены:"));
            assert!(!text.contains("Tokens:"));
            assert!(text.ends_with("Example analysis.\n"));
        }
    }

    #[test]
    fn export_replaces_existing_file_and_does_not_leave_temp_files() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("sysinfo-ai-test-{}-{sequence}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("system_report.json");
        atomic_save(&path, b"old").unwrap();
        atomic_save(&path, "{\"test\":\"memory\"}".as_bytes()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"test\":\"memory\"}");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn missing_export_directory_returns_error() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!(
                "sysinfo-ai-missing-{}-{sequence}",
                std::process::id()
            ))
            .join("report.json");
        assert!(atomic_save(&path, b"test").is_err());
        assert!(!path.exists());
    }

    #[test]
    fn worker_panic_becomes_an_error() {
        let context = egui::Context::default();
        let job = spawn_job::<(), _>("panic-test", &context, || panic!("test panic")).unwrap();
        assert!(job
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err());
    }

    #[test]
    fn disconnected_worker_does_not_leave_busy_state() {
        let (sender, receiver) = mpsc::channel::<Result<()>>();
        drop(sender);
        let mut job = Some(Job {
            receiver,
            started: Instant::now(),
        });
        assert!(poll_job(&mut job).unwrap().is_err());
        assert!(job.is_none());
    }

    #[test]
    #[ignore = "Runs a real Windows/WMI inventory; run locally with --ignored --nocapture"]
    fn real_windows_collection_smoke_test() {
        let report = collector::collect_all().unwrap();
        assert!(report.cpu.logical_cores > 0);
        assert!(report.memory.total.bytes > 0);
        let json = report.json_for_ai(PrivacyOptions::default()).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&json)
            .unwrap()
            .is_object());
        for warning in report.warnings {
            println!("{}: {}", warning.component, warning.message);
        }
    }

    #[test]
    fn language_gui_english_request_preserves_privacy_and_local_report() {
        let mut report = SystemReport::default();
        report.os.hostname = Some("private-host".to_owned());
        report.limitations.push("Это разовый снимок, а не бенчмарк. По текущей загрузке нельзя доказать постоянное узкое место.".to_owned());
        report.warnings.push(collector::CollectionWarning {
            component: "test".into(),
            message: "private-error".into(),
        });
        let request = prepare_analysis_request(
            &report,
            PrivacyOptions::default(),
            "test/model",
            Language::En,
            180,
            3072,
            true,
        )
        .unwrap();
        assert_eq!(request.language, ai::AnalysisLanguage::English);
        assert!(!request.report_json.contains("private-host"));
        assert!(!request.report_json.contains("private-error"));
        assert!(!request
            .report_json
            .chars()
            .any(|ch| ('\u{0400}'..='\u{052f}').contains(&ch)));
        assert!(request.report_json.contains("not a benchmark"));
        assert_eq!(report.os.hostname.as_deref(), Some("private-host"));
        assert_eq!(report.warnings[0].message, "private-error");
    }

    #[test]
    fn language_each_new_request_uses_the_current_gui_selection() {
        let report = SystemReport::default();
        for language in [Language::En, Language::Ru, Language::En] {
            let request = prepare_analysis_request(
                &report,
                PrivacyOptions::default(),
                "test/model",
                language,
                180,
                3072,
                true,
            )
            .unwrap();
            let expected = ai::AnalysisLanguage::from(language);
            assert_eq!(request.language, expected);
            assert_eq!(
                request.to_api_body().unwrap()["messages"][0]["content"],
                expected.system_prompt()
            );
            assert!(Language::from(expected) == language);
        }
    }

    #[test]
    fn language_pending_send_captures_request_language_without_duplicate_field() {
        let report = SystemReport::default();
        let request = prepare_analysis_request(
            &report,
            PrivacyOptions::default(),
            "test/model",
            Language::En,
            180,
            3072,
            true,
        )
        .unwrap();
        let pending = PendingSend {
            request,
            generation: 1,
            timestamp: 123,
            privacy: PrivacyOptions::default(),
            confirmed: false,
        };
        // Subsequent UI selection changes produce a different request without
        // mutating the payload shown in this existing confirmation dialog.
        let next = prepare_analysis_request(
            &report,
            PrivacyOptions::default(),
            "test/model",
            Language::Ru,
            180,
            3072,
            true,
        )
        .unwrap();
        assert_eq!(next.language, ai::AnalysisLanguage::Russian);
        assert_eq!(pending.request.language, ai::AnalysisLanguage::English);
        assert!(pending
            .request
            .user_message()
            .contains("ONLY in English (en)"));
    }

    #[test]
    fn language_markdown_metadata_records_the_requested_language() {
        let en = format_analysis(&analysis_fixture(Language::En, ai::Usage::default()));
        assert!(en.contains("Requested response language: English (en)."));
        assert!(!en.contains("Язык запроса"));
        let ru = format_analysis(&analysis_fixture(Language::Ru, ai::Usage::default()));
        assert!(ru.contains("Язык запроса: Русский (ru)."));
    }
}
