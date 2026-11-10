#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use omnismart::{
    ReaderContext,
    sle4428::{MEMORY_SIZE, SynchronousApi, WRITE_LIMIT, WriteRequest, parse_hex, parse_pin},
};
use std::{
    ffi::CString,
    io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};

fn main() -> eframe::Result {
    eframe::run_native(
        "OmniSmart - SLE4428",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([850.0, 800.0]),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::new(CardApp::default()))),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Text,
    Hex,
}

struct WriteJob {
    reader: CString,
    dll: PathBuf,
    request: WriteRequest,
    pin: [u8; 2],
}

enum Operation {
    Readers,
    Read { reader: CString, dll: PathBuf },
    Write(WriteJob),
}

enum Output {
    Readers(Vec<CString>),
    Memory(Box<[u8; MEMORY_SIZE]>),
    Written { address: u32, length: usize },
}

fn execute(operation: Operation) -> io::Result<Output> {
    match operation {
        Operation::Readers => Ok(Output::Readers(ReaderContext::establish()?.readers()?)),
        Operation::Read { reader, dll } => Ok(Output::Memory(Box::new(
            SynchronousApi::load(&dll)?.read(&reader)?,
        ))),
        Operation::Write(job) => {
            SynchronousApi::load(&job.dll)?.write(&job.reader, &job.request, job.pin)?;
            Ok(Output::Written {
                address: job.request.address(),
                length: job.request.data().len(),
            })
        }
    }
}

fn write_request(address: &str, data: &str, encoding: Encoding) -> io::Result<WriteRequest> {
    let address = u32::from_str_radix(address.strip_prefix("0x").unwrap_or(address), 16)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Address must be hexadecimal."))?;
    let bytes = match encoding {
        Encoding::Text => data.as_bytes().to_vec(),
        Encoding::Hex => parse_hex(data)?,
    };
    WriteRequest::new(address, bytes)
}

fn text_preview<'a>(memory: &'a [u8], address: &str, length: &str) -> io::Result<&'a str> {
    let address = usize::from_str_radix(address.strip_prefix("0x").unwrap_or(address), 16)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Preview address must be hex."))?;
    let length: usize = length.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Preview length must be a decimal byte count.",
        )
    })?;
    let end = address.checked_add(length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Preview range exceeds card memory.",
        )
    })?;
    let bytes = memory
        .get(address..end)
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Choose a nonempty preview range within 0000..03FF.",
            )
        })?;
    std::str::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "This range is not valid UTF-8 text. Use the hex view or adjust the range.",
        )
    })
}

struct CardApp {
    dll: String,
    readers: Vec<CString>,
    selected: Option<usize>,
    memory: Option<Box<[u8; MEMORY_SIZE]>>,
    address: String,
    data: String,
    field_length: String,
    replace: bool,
    encoding: Encoding,
    pin: String,
    preview_address: String,
    preview_length: String,
    pending: Option<WriteJob>,
    confirmation: String,
    worker: Option<Receiver<io::Result<Output>>>,
    status: String,
    error: Option<String>,
}

impl Default for CardApp {
    fn default() -> Self {
        let dll = if cfg!(windows) {
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
                .join("System32")
                .join(if cfg!(target_pointer_width = "64") {
                    "scardsynx64.dll"
                } else {
                    "scardsyn.dll"
                })
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        Self {
            dll,
            readers: Vec::new(),
            selected: None,
            memory: None,
            address: "0020".to_owned(),
            data: String::new(),
            field_length: String::new(),
            replace: false,
            encoding: Encoding::Text,
            pin: String::new(),
            preview_address: "0020".to_owned(),
            preview_length: "64".to_owned(),
            pending: None,
            confirmation: String::new(),
            worker: None,
            status: String::new(),
            error: None,
        }
    }
}

impl CardApp {
    fn connection(&self) -> io::Result<(CString, PathBuf)> {
        let reader = self
            .selected
            .and_then(|index| self.readers.get(index))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Select a reader first."))?;
        let dll = Path::new(&self.dll);
        if !dll.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Enter the absolute path to a trusted HID synchronous API DLL.",
            ));
        }
        Ok((reader.clone(), dll.to_owned()))
    }

    fn start(&mut self, operation: Operation, ctx: &egui::Context) {
        if self.worker.is_some() {
            self.error = Some("Wait for the current card operation to finish.".to_owned());
            return;
        }
        self.memory = None;
        self.error = None;
        self.status = "Working...".to_owned();
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        match std::thread::Builder::new()
            .name("smart-card".to_owned())
            .spawn(move || {
                let result = execute(operation);
                if sender.send(result).is_ok() {
                    ctx.request_repaint();
                }
            }) {
            Ok(_) => self.worker = Some(receiver),
            Err(error) => {
                self.status.clear();
                self.error = Some(format!(
                    "Cannot start card worker: {error}. No operation was started."
                ));
            }
        }
    }

    fn poll(&mut self) {
        let result = match self.worker.as_ref().map(Receiver::try_recv) {
            None | Some(Err(TryRecvError::Empty)) => return,
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Disconnected)) => Err(io::Error::other(
                "Card worker stopped unexpectedly. A write may be partially complete; do not blindly retry.",
            )),
        };
        self.worker = None;
        self.status.clear();
        match result {
            Ok(Output::Readers(readers)) => {
                self.selected = if readers.len() == 1 { Some(0) } else { None };
                self.status = if readers.is_empty() {
                    "No PC/SC readers found. Check the connection and driver.".to_owned()
                } else {
                    format!("{} reader(s)", readers.len())
                };
                self.readers = readers;
            }
            Ok(Output::Memory(memory)) => {
                self.memory = Some(memory);
                self.status = "Read 1024 bytes.".to_owned();
            }
            Ok(Output::Written { address, length }) => {
                self.status = format!("Write verified: {length} bytes at {address:04X}.");
            }
            Err(error) => {
                self.memory = None;
                self.error = Some(format!(
                    "{error}\nNo automatic retry. If writing had begun, memory may be partially modified."
                ));
            }
        }
    }

    fn write_request(&self) -> io::Result<WriteRequest> {
        let request = write_request(&self.address, &self.data, self.encoding)?;
        if self.replace {
            if !self.field_length.trim().is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Replacement and a fixed field length cannot be combined.",
                ));
            }
            return request.replace_to_end();
        }
        if self.field_length.trim().is_empty() {
            return Ok(request);
        }
        let length = self.field_length.trim().parse::<usize>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Field length must be a decimal byte count, or blank for no padding.",
            )
        })?;
        request.pad_to(length)
    }

    fn review(&mut self) -> io::Result<()> {
        let (reader, dll) = self.connection()?;
        let request = self.write_request()?;
        let pin = parse_pin(&self.pin)?;
        self.pending = Some(WriteJob {
            reader,
            dll,
            request,
            pin,
        });
        self.pin.clear();
        self.confirmation.clear();
        self.error = None;
        Ok(())
    }

    fn ui(&mut self, ctx: &egui::Context) {
        let busy = self.worker.is_some();
        egui::TopBottomPanel::top("card-controls").show(ctx, |ui| {
            ui.add_enabled_ui(!busy && self.pending.is_none() && cfg!(windows), |ui| {
                ui.horizontal(|ui| {
                    ui.label("HID DLL");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.dll)
                                .desired_width(ui.available_width()),
                        )
                        .changed()
                    {
                        self.memory = None;
                        self.pin.clear();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Refresh readers").clicked() {
                        self.readers.clear();
                        self.selected = None;
                        self.pin.clear();
                        self.start(Operation::Readers, ctx);
                    }
                    let old = self.selected;
                    egui::ComboBox::from_id_salt("reader")
                        .selected_text(
                            self.selected
                                .and_then(|index| self.readers.get(index))
                                .map(|reader| reader.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "Select reader".to_owned()),
                        )
                        .show_ui(ui, |ui| {
                            for (index, reader) in self.readers.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.selected,
                                    Some(index),
                                    reader.to_string_lossy(),
                                );
                            }
                        });
                    if old != self.selected {
                        self.memory = None;
                        self.pin.clear();
                    }
                    if ui.button("Read memory").clicked() {
                        match self.connection() {
                            Ok((reader, dll)) => self.start(Operation::Read { reader, dll }, ctx),
                            Err(error) => self.error = Some(error.to_string()),
                        }
                    }
                });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label("Write address (hex)");
                    ui.add(egui::TextEdit::singleline(&mut self.address).desired_width(80.0));
                    ui.radio_value(&mut self.encoding, Encoding::Text, "UTF-8 text");
                    ui.radio_value(&mut self.encoding, Encoding::Hex, "Hex bytes");
                    if ui
                        .checkbox(&mut self.replace, "Replace through 03FC")
                        .changed()
                        && self.replace
                    {
                        self.field_length.clear();
                    }
                    ui.add_enabled_ui(!self.replace, |ui| {
                        ui.label("Field length (bytes)");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.field_length).desired_width(80.0),
                        );
                    });
                });
                ui.add(
                    egui::TextEdit::multiline(&mut self.data)
                        .hint_text("Data")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label("PIN (hex)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pin)
                            .password(true)
                            .desired_width(100.0),
                    );
                    if ui.button("Review write...").clicked()
                        && let Err(error) = self.review()
                    {
                        self.pin.clear();
                        self.error = Some(error.to_string());
                    }
                    if !self.data.is_empty() {
                        match self.write_request() {
                            Ok(request) => {
                                ui.small(format!(
                                    "{} bytes / {} available; {} zero-fill",
                                    request.data().len(),
                                    WRITE_LIMIT - request.address() as usize,
                                    request.padding_len()
                                ));
                            }
                            Err(error) => {
                                ui.colored_label(egui::Color32::RED, error.to_string());
                            }
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Preview address (hex)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.preview_address).desired_width(70.0),
                    );
                    ui.label("Preview length (bytes)");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.preview_length).desired_width(70.0),
                    );
                });
            });
        });
        egui::TopBottomPanel::bottom("card-status").show(ctx, |ui| {
            if busy {
                ui.spinner();
            }
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
            if let Some(error) = &self.error {
                ui.colored_label(egui::Color32::RED, error);
            }
            if !cfg!(windows) {
                ui.colored_label(
                    egui::Color32::RED,
                    "SLE4428 access is supported only on Windows.",
                );
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(memory) = &self.memory {
                ui.columns(2, |columns| {
                    columns[0].heading("Read snapshot");
                    egui::ScrollArea::both()
                        .id_salt("snapshot-hex")
                        .auto_shrink([false, false])
                        .show(&mut columns[0], |ui| {
                            for (row, bytes) in memory.chunks(16).enumerate() {
                                let hex = bytes
                                    .iter()
                                    .map(|byte| format!("{byte:02X}"))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!("{:04X}: {hex}", row * 16))
                                            .monospace(),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Extend),
                                );
                            }
                        });
                    columns[1].heading("Readable string");
                    egui::ScrollArea::vertical()
                        .id_salt("snapshot-text")
                        .auto_shrink([false, false])
                        .show(&mut columns[1], |ui| {
                            match text_preview(
                                memory.as_slice(),
                                &self.preview_address,
                                &self.preview_length,
                            ) {
                                Ok(text) => {
                                    ui.add(egui::Label::new(text).wrap());
                                }
                                Err(error) => {
                                    ui.colored_label(egui::Color32::RED, error.to_string());
                                }
                            }
                        });
                });
            }
        });
        self.confirmation_window(ctx);
    }

    fn confirmation_window(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.pending else { return };
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Confirm irreversible memory overwrite")
            .collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(format!("Reader: {}", job.reader.to_string_lossy()));
                ui.label(format!("DLL: {}", job.dll.display()));
                ui.label(format!("Overwrite {} bytes at {:04X}..{:04X}.",
                    job.request.data().len(), job.request.address(),
                    job.request.address() + job.request.data().len() as u32 - 1));
                if job.request.padding_len() > 0 {
                    ui.label(format!("Includes {} trailing 00 bytes; existing contents in that range will be cleared.",
                        job.request.padding_len()));
                }
                ui.label("Ensure this is the intended card. Do not remove it during the operation.");
                ui.label("Wrong PINs consume retries. Failed writes may be partial; there is no rollback.");
                ui.label("Type WRITE to confirm this operation:");
                ui.text_edit_singleline(&mut self.confirmation);
                ui.horizontal(|ui| {
                    confirm = ui.add_enabled(self.confirmation == "WRITE", egui::Button::new("Write and verify")).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if cancel {
            self.pending = None;
            self.confirmation.clear();
            self.status = "Write cancelled. No PIN attempt or write was made.".to_owned();
        } else if confirm {
            match self.take_confirmed_write() {
                Ok(job) => self.start(Operation::Write(job), ctx),
                Err(error) => self.error = Some(error.to_string()),
            }
        }
    }

    fn take_confirmed_write(&mut self) -> io::Result<WriteJob> {
        if self.confirmation != "WRITE" {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Type WRITE to confirm. No PIN attempt or write was made.",
            ));
        }
        let job = self.pending.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "No write is pending confirmation.",
            )
        })?;
        self.confirmation.clear();
        Ok(job)
    }
}

impl eframe::App for CardApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        if self.worker.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
            if ctx.input(|input| input.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.status = "Wait for the card operation to finish before closing.".to_owned();
            }
        }
        self.ui(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_writes_count_utf8_bytes_and_enforce_security_boundaries() {
        let request = write_request("0020", "Hello \u{20AC}", Encoding::Text).unwrap();
        assert_eq!(request.data(), "Hello \u{20AC}".as_bytes());
        assert_eq!(request.data().len(), 9);
        assert!(write_request("0020", &"A".repeat(989), Encoding::Text).is_ok());
        assert!(write_request("0020", &"A".repeat(990), Encoding::Text).is_err());
        assert!(write_request("03FD", "A", Encoding::Text).is_err());
        assert!(write_request("0020", "", Encoding::Text).is_err());
        assert!(write_request("bad address", "A", Encoding::Text).is_err());
        assert!(write_request("0020", &"\u{20AC}".repeat(329), Encoding::Text).is_ok());
        assert!(write_request("0020", &"\u{20AC}".repeat(330), Encoding::Text).is_err());
    }

    #[test]
    fn hex_writes_share_cli_validation() {
        assert_eq!(
            write_request("0x20", "AA bb 01", Encoding::Hex)
                .unwrap()
                .data(),
            [0xAA, 0xBB, 1]
        );
        assert!(write_request("20", "A", Encoding::Hex).is_err());
    }

    #[test]
    fn previews_require_explicit_valid_ranges_and_utf8() {
        let memory = [b'H', b'i', 0xFF];
        assert_eq!(text_preview(&memory, "0", "2").unwrap(), "Hi");
        for (address, length) in [
            ("0", "3"),
            ("0", "0"),
            ("3", "1"),
            ("0", "4"),
            ("0", "x"),
            ("FFFFFFFFFFFFFFFF", "2"),
        ] {
            assert!(text_preview(&memory, address, length).is_err());
        }
    }

    #[test]
    fn review_freezes_target_and_payload_without_accessing_card() {
        let mut app = CardApp {
            readers: vec![CString::new("Synthetic reader").unwrap()],
            selected: Some(0),
            dll: std::env::temp_dir()
                .join("not-loaded.dll")
                .to_string_lossy()
                .into_owned(),
            data: "Hello".to_owned(),
            pin: "1234".to_owned(),
            ..Default::default()
        };
        app.review().unwrap();
        assert!(app.pin.is_empty());
        assert!(app.worker.is_none());
        let pending = app.pending.unwrap();
        assert_eq!(pending.pin, [0x12, 0x34]);
        assert_eq!(pending.request.data(), b"Hello");
        assert_eq!(pending.request.address(), 0x20);
    }

    #[test]
    fn failed_workers_clear_stale_memory_and_report_errors() {
        let (sender, receiver) = mpsc::channel();
        let mut app = CardApp {
            worker: Some(receiver),
            memory: Some(Box::new([0; MEMORY_SIZE])),
            ..Default::default()
        };
        sender
            .send(Err(io::Error::other("Synthetic failure")))
            .unwrap();
        app.poll();
        assert!(app.worker.is_none());
        assert!(app.memory.is_none());
        assert!(app.error.unwrap().contains("Synthetic failure"));
    }

    #[test]
    fn headless_ui_does_not_start_hardware_operations() {
        let mut app = CardApp::default();
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| app.ui(ctx));
        assert!(app.worker.is_none());
        assert!(app.pending.is_none());
    }

    #[test]
    fn snapshot_renders_readable_text_to_the_right_of_hex() {
        let mut memory = Box::new([0; MEMORY_SIZE]);
        memory[0x20..0x25].copy_from_slice(b"Hello");
        let mut app = CardApp {
            memory: Some(memory),
            preview_length: "5".to_owned(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 1400.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input.clone(), |ctx| app.ui(ctx));
        let output = ctx.run(input, |ctx| app.ui(ctx));
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some((text.galley.job.text.as_str(), text.pos.x))
                } else {
                    None
                }
            })
            .collect();
        let hex_x = texts
            .iter()
            .find(|(text, _)| *text == "Read snapshot")
            .unwrap()
            .1;
        let string_x = texts
            .iter()
            .find(|(text, _)| *text == "Readable string")
            .unwrap()
            .1;
        assert!(string_x > hex_x);
        assert!(texts.iter().any(|(text, _)| *text == "Hello"));
        let text_y = |label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape
                        && text.galley.job.text == label
                    {
                        Some(text.pos.y)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| panic!("Missing UI label: {label}"))
        };
        for label in [
            "HID DLL",
            "Write address (hex)",
            "PIN (hex)",
            "Preview address (hex)",
            "Preview length (bytes)",
        ] {
            assert!(
                text_y(label) < text_y("Read snapshot"),
                "{label} ({}) must be above the snapshot ({})",
                text_y(label),
                text_y("Read snapshot")
            );
        }
        assert!(
            !texts.iter().any(|(text, _)| text.contains("Shorter writes")
                || text.contains("EMV remains available")
                || text.contains("Use only your own card"))
        );
        assert!(app.worker.is_none());
    }

    #[test]
    fn confirmation_consumes_exactly_one_frozen_write() {
        let mut app = CardApp {
            readers: vec![CString::new("Synthetic reader").unwrap()],
            selected: Some(0),
            dll: std::env::temp_dir()
                .join("not-loaded.dll")
                .to_string_lossy()
                .into_owned(),
            data: "Hello".to_owned(),
            pin: "1234".to_owned(),
            ..Default::default()
        };
        app.review().unwrap();
        app.data = "Changed after review".to_owned();
        for value in ["", "yes", "write", "WRITE "] {
            app.confirmation = value.to_owned();
            assert!(app.take_confirmed_write().is_err());
            assert!(app.pending.is_some());
            assert!(app.worker.is_none());
        }
        app.confirmation = "WRITE".to_owned();
        let job = app.take_confirmed_write().unwrap();
        assert_eq!(job.request.data(), b"Hello");
        assert!(app.pending.is_none());
        assert!(app.confirmation.is_empty());
        assert!(app.take_confirmed_write().is_err());
    }

    #[test]
    fn review_rejects_bad_pin_before_creating_a_write_job() {
        let mut app = CardApp {
            readers: vec![CString::new("Synthetic reader").unwrap()],
            selected: Some(0),
            dll: std::env::temp_dir()
                .join("not-loaded.dll")
                .to_string_lossy()
                .into_owned(),
            data: "Hello".to_owned(),
            pin: "secret-invalid".to_owned(),
            ..Default::default()
        };
        assert!(app.review().is_err());
        assert!(app.pending.is_none());
        assert!(app.worker.is_none());
    }

    #[test]
    fn disconnected_worker_reports_failure_instead_of_success() {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let mut app = CardApp {
            worker: Some(receiver),
            ..Default::default()
        };
        app.poll();
        assert!(app.worker.is_none());
        assert!(app.error.unwrap().contains("stopped unexpectedly"));
    }

    #[test]
    fn replacement_is_opt_in_and_cannot_mix_with_fixed_fields() {
        let mut app = CardApp {
            data: "Hello".to_owned(),
            ..Default::default()
        };
        assert!(!app.replace);
        assert_eq!(app.write_request().unwrap().data(), b"Hello");
        app.replace = true;
        let request = app.write_request().unwrap();
        assert_eq!(request.data().len(), 989);
        assert_eq!(&request.data()[..5], b"Hello");
        assert!(request.data()[5..].iter().all(|byte| *byte == 0));
        app.field_length = "64".to_owned();
        assert!(app.write_request().is_err());
    }

    #[test]
    fn gui_field_length_counts_utf8_bytes_and_rejects_truncation() {
        let mut app = CardApp {
            data: "\u{20AC}".to_owned(),
            field_length: "5".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            app.write_request().unwrap().data(),
            &[0xE2, 0x82, 0xAC, 0, 0]
        );
        for value in ["0", "2", "990", "0x20", "invalid", "-1"] {
            app.field_length = value.to_owned();
            assert!(app.write_request().is_err());
        }
        app.field_length.clear();
        assert_eq!(app.write_request().unwrap().data(), "\u{20AC}".as_bytes());
        app.encoding = Encoding::Hex;
        app.data = "AA BB".to_owned();
        app.field_length = "4".to_owned();
        assert_eq!(app.write_request().unwrap().data(), &[0xAA, 0xBB, 0, 0]);
    }

    #[test]
    fn reviewed_write_freezes_the_full_padded_range() {
        let mut app = CardApp {
            readers: vec![CString::new("Synthetic reader").unwrap()],
            selected: Some(0),
            dll: std::env::temp_dir()
                .join("not-loaded.dll")
                .to_string_lossy()
                .into_owned(),
            data: "Hi".to_owned(),
            field_length: "8".to_owned(),
            pin: "1234".to_owned(),
            ..Default::default()
        };
        app.review().unwrap();
        app.field_length = "100".to_owned();
        app.confirmation = "WRITE".to_owned();
        let job = app.take_confirmed_write().unwrap();
        assert_eq!(job.request.data(), b"Hi\0\0\0\0\0\0");
        assert_eq!(job.request.padding_len(), 6);
        assert!(app.worker.is_none());
    }
}
