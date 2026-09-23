<h1 align="center">SysInfo AI</h1>

<p align="center">Understand your computer, not just its specifications.</p>

<p align="center">
  <a href="./README.md">Русский</a> · <strong>English</strong>
</p>

<p align="center">
  <a href="./dist/sysinfo-ai.exe?raw=true">
    <img src="https://img.shields.io/badge/Download-Windows%20x64-2563EB?style=for-the-badge" alt="Download SysInfo AI for Windows x64">
  </a>
</p>

<p align="center">Rust · egui/eframe · WMI + DXGI · OpenRouter</p>

**SysInfo AI** is a native Windows application that collects computer information and helps you understand what it means: suitable workloads, potential bottlenecks, and improvements that may actually be useful.

Hardware inventory runs locally. AI analysis is a separate, optional feature: a selected copy of the report is sent to OpenRouter only after your confirmation. The interface does not use WebView, Electron, HTML, or JavaScript.

## Download and run

**[Download sysinfo-ai.exe](./dist/sysinfo-ai.exe?raw=true)** — the file in the `dist` directory on the current repository branch.

Save the `.exe` to a folder of your choice and run it. No installer, Rust installation, or development tools are needed to run a prebuilt binary.

The target platform is **Windows 10/11 x64**. The interface requires a working graphics driver with OpenGL support; some information is collected through WMI. Internet access and an OpenRouter API key are needed only for AI analysis. Loading the model catalog through its dedicated button requires internet access but no key. Windows system components are still required: “one `.exe`” does not mean independence from the operating system.

## Features

- **Hardware organized by category.** Overview, OS, CPU, memory, storage, GPU, motherboard and BIOS, network, processes, and the full JSON. Cards use the available window width, and long sections scroll.
- **Human-friendly analysis.** The model is instructed to explain the configuration, assess common workloads, identify potential limitations, give 2–3 practical suggestions, and acknowledge missing information.
- **Interface options.** Light and dark themes, Russian and English. The language of a new AI request follows the selected interface language and is shown in the confirmation dialog.
- **Export and responsiveness.** Copy JSON and analysis, save files, and edit Markdown. Collection, HTTP requests, and file writes run in background threads.

## Information collected

| Category | Data |
| --- | --- |
| OS | Name and versions, hostname, architecture, uptime, and boot time. |
| CPU | Brand, physical and logical cores, reported frequency, usage, and logical processor details; additional socket and cache information through WMI. |
| RAM and swap | Total, used, available, and free memory; swap figures. |
| RAM modules | Slot, bank, manufacturer, part number, serial number, capacity, reported speeds, type and form-factor codes, and data widths. |
| Volumes and physical drives | Mount points, capacity and free space, file system, removability; drive models, firmware, interfaces, and WMI status. |
| GPU | Name, video processor, driver version and date, reported display mode; a separate DXGI inventory with dedicated and shared memory. |
| Motherboard and BIOS | Manufacturer, model, versions, serial numbers, and BIOS release date. |
| Network | Interfaces, MAC addresses, and cumulative received/transmitted byte, packet, and error counters. |
| Processes | Top 10 by memory: name, PID, resident memory, and CPU usage normalized to the whole system. |

The full report also contains collection timing, warnings, and source limitations. Unknown values may appear as `null`; this does not necessarily mean that hardware is absent.

## Usage

### Local report

A local scan starts when the application opens. Select a category in the **System** tab. **Refresh data** creates a new snapshot; the application is not a continuous hardware monitor.

Use **Copy JSON** or **Save to file** to export the report. The complete JSON is also available in its own section.

### AI analysis

1. Select **RU** or **EN** before starting a new analysis, then open the **AI analysis** tab.
2. Enter an OpenRouter API key and select a model or enter its ID manually. You can load the catalog using its dedicated button; built-in examples do not guarantee model availability.
3. Configure identifier redaction, process inclusion, and request parameters. Click **Analyze with AI**.
4. Review the model, response language, and exact JSON in the confirmation dialog, check the consent box, and click **Send**. You can edit, copy, and save the returned Markdown.

The language is fixed for the confirmed request. Switching RU/EN does not translate an existing result or change a request already running. A model may ignore the language instruction; obtaining a new result requires another confirmed request.

### Saved files

| File | Contents |
| --- | --- |
| `system_report.json` | The complete local report. |
| `ai_analysis.md` | The current analysis text, including your edits and response metadata. |

Files are saved next to the `.exe` by default. You can enter another absolute path in the dialog; its directory must exist and be writable. A warning is shown before replacing an existing file.

## Privacy

**The application does not write the API key to disk.** The key field is cleared after a confirmed request starts. Settings are not persisted, and AI requests never run automatically. This does not promise secure erasure of all key copies from memory, the clipboard, or operating-system dumps.

For AI requests, hostname, MAC addresses, serial numbers, interface and volume names, and mount paths are hidden by default. Detailed error messages are replaced with neutral descriptions, and the process list is omitted. The complete local report remains unchanged. Hardware models remain in the uploaded data, so hiding identifiers does not guarantee anonymity.

> [!IMPORTANT]
> A confirmed request sends data to OpenRouter and the model provider and may be billable. The `data_collection=deny` filter is not a universal zero-retention guarantee. Review service and provider settings; do not publish API keys or unredacted reports.

There are no automatic retries of billable requests. A timeout or closing the application does not guarantee cancellation of a request already accepted by the provider. If a new request fails, the previous successful analysis remains in the interface.

## Limitations

A configuration snapshot is **not a benchmark or a hardware health test**. One-time utilization does not prove a persistent bottleneck. Temperatures, SMART, SSD wear, PSU condition, and GPU utilization are not measured.

Frequencies are shown as reported by the source. Memory uses binary GiB/MiB units. Shared GPU memory is not added to dedicated VRAM, and WMI `AdapterRAM` is not treated as a reliable exact value for large video-memory capacities. Do not count volumes and physical drives twice or interpret cumulative network traffic as internet speed.

Data availability depends on the operating system, drivers, firmware, and permissions. AI suggestions may contain errors: verify component compatibility before buying hardware or changing the configuration.

## Build from source

Building on Windows requires Rust stable with the `x86_64-pc-windows-msvc` target, Microsoft C++ Build Tools with desktop C++ components, and the Windows SDK. Toolchain settings are in [`rust-toolchain.toml`](./rust-toolchain.toml).

Run this command from the project root:

```powershell
.\build.cmd
```

The script creates `Cargo.lock` if it is missing, formats the source, runs `cargo check`, runs regular unit tests, and builds the release binary. It stops if a required step fails. After a successful build, the executable is copied to:

```text
dist/sysinfo-ai.exe
```

Keep the generated `Cargo.lock` in the repository. The release profile uses `lto = true`, `strip = true`, and static CRT linkage for Windows MSVC; the interface uses Glow/OpenGL.

Additional checks after dependencies have been resolved:

```powershell
cargo test --locked --all-targets
cargo clippy --locked --all-targets
```

Separate test using real Windows hardware:

```powershell
cargo test --locked real_windows_collection_smoke_test -- --ignored --nocapture
```

Tests being present in the source does not mean that every published build has passed all checks.

## Project structure

```text
.
├── README.md
├── README.en.md
├── Cargo.toml
├── Cargo.lock                  # generated during dependency resolution
├── rust-toolchain.toml
├── .cargo/config.toml
├── build.cmd
├── build.ps1
├── src/
│   ├── main.rs                 # GUI, state, background jobs, export
│   ├── collector.rs            # sysinfo, WMI, DXGI, privacy-filtered report
│   └── ai.rs                   # OpenRouter, request languages, response handling
└── dist/
    └── sysinfo-ai.exe          # compiled binary for download
```

## Report an issue

In GitHub Issues, include your Windows version, the build you are using, reproduction steps, and the error message. For a response-language issue, also include the selected RU/EN setting and model ID. Remove API keys, hostname, MAC addresses, and serial numbers before sharing screenshots or reports.
