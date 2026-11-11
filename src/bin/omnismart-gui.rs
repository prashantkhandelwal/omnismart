#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use omnismart::{
    ReaderContext, emv,
    sle4428::{MEMORY_SIZE, SynchronousApi, WRITE_LIMIT, WriteRequest, parse_hex, parse_pin},
};
use std::{
    ffi::CString,
    io,
    ops::Range,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};

fn main() -> eframe::Result {
    eframe::run_native(
        "OmniSmart",
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum CardMode {
    Sle4428,
    Emv,
}

struct WriteJob {
    reader: CString,
    dll: PathBuf,
    request: WriteRequest,
    pin: [u8; 2],
}

enum Operation {
    Readers,
    Read {
        reader: CString,
        dll: PathBuf,
    },
    ReadEmv {
        reader: CString,
        aid: Option<Vec<u8>>,
        show_pan: bool,
    },
    Write(WriteJob),
}

enum Output {
    Readers(Vec<CString>),
    Memory(Box<[u8; MEMORY_SIZE]>),
    Emv(String),
    Written { address: u32, length: usize },
}

fn execute(operation: Operation) -> io::Result<Output> {
    match operation {
        Operation::Readers => Ok(Output::Readers(ReaderContext::establish()?.readers()?)),
        Operation::Read { reader, dll } => Ok(Output::Memory(Box::new(
            SynchronousApi::load(&dll)?.read(&reader)?,
        ))),
        Operation::ReadEmv {
            reader,
            aid,
            show_pan,
        } => {
            let context = ReaderContext::establish()?;
            let mut card = context.connect(&reader)?;
            let report =
                card.with_transaction(|transmit| emv_report(transmit, aid.as_deref(), show_pan))?;
            Ok(Output::Emv(report))
        }
        Operation::Write(job) => {
            SynchronousApi::load(&job.dll)?.write(&job.reader, &job.request, job.pin)?;
            Ok(Output::Written {
                address: job.request.address(),
                length: job.request.data().len(),
            })
        }
    }
}

fn emv_report(
    transmit: impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
    aid: Option<&[u8]>,
    show_pan: bool,
) -> io::Result<String> {
    let mut buffer = Vec::new();
    emv::read_information_to(transmit, aid, show_pan, &mut buffer)?;
    String::from_utf8(buffer).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
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

fn text_preview<'a>(memory: &'a [u8], address: &str, length: &str) -> io::Result<(usize, &'a str)> {
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
    let text = std::str::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "This range is not valid UTF-8 text. Use the hex view or adjust the range.",
        )
    })?;
    Ok((address, text))
}

struct SnapshotText {
    text: String,
    bytes: Vec<Option<Range<usize>>>,
}

impl SnapshotText {
    fn hex(memory: &[u8]) -> Self {
        let mut result = Self {
            text: String::new(),
            bytes: Vec::new(),
        };
        for (row, bytes) in memory.chunks(16).enumerate() {
            if row > 0 {
                result.text.push('\n');
                result.bytes.push(None);
            }
            let prefix = format!("{:04X}: ", row * 16);
            result.bytes.extend(std::iter::repeat_n(None, prefix.len()));
            result.text.push_str(&prefix);
            for (column, byte) in bytes.iter().enumerate() {
                if column > 0 {
                    result.text.push(' ');
                    result.bytes.push(None);
                }
                let address = row * 16 + column;
                result.text.push_str(&format!("{byte:02X}"));
                result
                    .bytes
                    .extend(std::iter::repeat_n(Some(address..address + 1), 2));
            }
        }
        result
    }

    fn readable(address: usize, text: &str) -> Self {
        Self {
            text: text.to_owned(),
            bytes: text
                .char_indices()
                .map(|(index, ch)| Some(address + index..address + index + ch.len_utf8()))
                .collect(),
        }
    }

    fn selected_bytes(&self, characters: Range<usize>) -> Option<Range<usize>> {
        let mut ranges = self.bytes[characters].iter().flatten();
        let first = ranges.next()?;
        let end = ranges.last().unwrap_or(first).end;
        Some(first.start..end)
    }

    fn clicked_hex_characters(
        &self,
        galley: &egui::Galley,
        position: egui::Vec2,
    ) -> Option<Range<usize>> {
        let cursor = galley.cursor_from_pos(position);
        let caret = galley.pos_from_cursor(cursor);
        let index = if position.x < caret.left() {
            cursor.index.checked_sub(1)?
        } else {
            cursor.index
        };
        let start = galley.pos_from_cursor(egui::text::CCursor::new(index));
        let end = galley.pos_from_cursor(egui::text::CCursor::new(index + 1));
        let bounds = egui::Rect::from_min_max(start.left_top(), end.left_bottom());
        if !bounds.contains(position.to_pos2()) {
            return None;
        }
        let byte = self.bytes.get(index)?.as_ref()?;
        let first = if index > 0 && self.bytes[index - 1].as_ref() == Some(byte) {
            index - 1
        } else {
            index
        };
        Some(first..first + 2)
    }

    fn layout(
        &self,
        font_id: egui::FontId,
        color: egui::Color32,
        background: egui::Color32,
        selection: Option<&Range<usize>>,
        wrap_width: f32,
    ) -> egui::text::LayoutJob {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap_width;
        for (ch, bytes) in self.text.chars().zip(&self.bytes) {
            let selected = bytes.as_ref().is_some_and(|bytes| {
                selection.is_some_and(|selection| {
                    bytes.start < selection.end && selection.start < bytes.end
                })
            });
            job.append(
                ch.encode_utf8(&mut [0; 4]),
                0.0,
                egui::TextFormat {
                    font_id: font_id.clone(),
                    color,
                    background: if selected {
                        background
                    } else {
                        egui::Color32::TRANSPARENT
                    },
                    ..Default::default()
                },
            );
        }
        job
    }
}

fn snapshot_view(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    snapshot: &SnapshotText,
    selection: &mut Option<Range<usize>>,
    wrap: bool,
) -> egui::text_edit::TextEditOutput {
    let font = if wrap {
        egui::TextStyle::Body
    } else {
        egui::TextStyle::Monospace
    };
    let mut layouter = |ui: &egui::Ui, _: &dyn egui::TextBuffer, width: f32| {
        let job = snapshot.layout(
            font.resolve(ui.style()),
            ui.visuals().text_color(),
            ui.visuals().selection.bg_fill,
            selection.as_ref(),
            if wrap { width } else { f32::INFINITY },
        );
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let mut text = snapshot.text.as_str();
    let mut output = egui::TextEdit::multiline(&mut text)
        .id_salt(id)
        .font(font.clone())
        .frame(false)
        .desired_rows(1)
        .desired_width(if wrap { f32::INFINITY } else { 0.0 })
        .layouter(&mut layouter)
        .show(ui);
    if !wrap
        && output.response.clicked_by(egui::PointerButton::Primary)
        && !output.response.double_clicked()
        && !output.response.triple_clicked()
        && !ui.input(|input| input.modifiers.shift)
        && let Some(position) = output.response.interact_pointer_pos()
        && let Some(characters) =
            snapshot.clicked_hex_characters(&output.galley, position - output.galley_pos)
    {
        let cursor = egui::text::CCursorRange::two(
            egui::text::CCursor::new(characters.start),
            egui::text::CCursor::new(characters.end),
        );
        output.cursor_range = Some(cursor);
        output.state.cursor.set_char_range(Some(cursor));
        output.state.clone().store(ui.ctx(), output.response.id);
    }
    if output.response.has_focus()
        && let Some(cursor) = output.cursor_range
    {
        let next = snapshot.selected_bytes(cursor.as_sorted_char_range());
        if *selection != next {
            *selection = next;
            ui.ctx().request_repaint();
        }
    }
    output
}

struct CardApp {
    show_pan: bool,
    mode: CardMode,
    aid: String,
    emv: Option<String>,
    dll: String,
    readers: Vec<CString>,
    selected: Option<usize>,
    memory: Option<Box<[u8; MEMORY_SIZE]>>,
    snapshot_selection: Option<Range<usize>>,
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
            show_pan: false,
            mode: CardMode::Sle4428,
            aid: String::new(),
            emv: None,
            dll,
            readers: Vec::new(),
            selected: None,
            memory: None,
            snapshot_selection: None,
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
    fn reader(&self) -> io::Result<CString> {
        self.selected
            .and_then(|index| self.readers.get(index))
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Select a reader first."))
    }

    fn connection(&self) -> io::Result<(CString, PathBuf)> {
        let reader = self.reader()?;
        let dll = Path::new(&self.dll);
        if !dll.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Enter the absolute path to a trusted HID synchronous API DLL.",
            ));
        }
        Ok((reader, dll.to_owned()))
    }

    fn read_operation(&self) -> io::Result<Operation> {
        match self.mode {
            CardMode::Sle4428 => {
                let (reader, dll) = self.connection()?;
                Ok(Operation::Read { reader, dll })
            }
            CardMode::Emv => Ok(Operation::ReadEmv {
                reader: self.reader()?,
                show_pan: self.show_pan,
                aid: if self.aid.trim().is_empty() {
                    None
                } else {
                    Some(emv::parse_aid(self.aid.trim())?)
                },
            }),
        }
    }

    fn clear_snapshot(&mut self) {
        self.memory = None;
        self.snapshot_selection = None;
        self.emv = None;
    }

    fn set_show_pan(&mut self, show_pan: bool) {
        if self.show_pan != show_pan {
            self.show_pan = show_pan;
            self.clear_snapshot();
            self.status = "Click Read EMV to apply PAN visibility.".to_owned();
        }
    }

    fn change_mode(&mut self, mode: CardMode) {
        if self.mode != mode {
            self.mode = mode;
            self.show_pan = false;
            self.clear_snapshot();
            self.pin.clear();
            self.status.clear();
            self.error = None;
        }
    }

    fn start(&mut self, operation: Operation, ctx: &egui::Context) {
        if self.worker.is_some() {
            self.error = Some("Wait for the current card operation to finish.".to_owned());
            return;
        }
        self.clear_snapshot();
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
            Ok(Output::Emv(report)) => {
                self.emv = Some(report);
                self.status = "EMV read complete.".to_owned();
            }
            Ok(Output::Written { address, length }) => {
                self.status = format!("Write verified: {length} bytes at {address:04X}.");
            }
            Err(error) => {
                self.clear_snapshot();
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
        if self.mode != CardMode::Sle4428 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "EMV mode is read-only.",
            ));
        }
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
            ui.add_enabled_ui(!busy && self.pending.is_none(), |ui| {
                let mut mode = self.mode;
                ui.horizontal(|ui| {
                    ui.label("Card");
                    ui.radio_value(&mut mode, CardMode::Sle4428, "SLE4428");
                    ui.radio_value(&mut mode, CardMode::Emv, "EMV (read-only)");
                });
                self.change_mode(mode);
                if self.mode == CardMode::Sle4428 {
                    ui.horizontal(|ui| {
                        ui.label("HID DLL");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.dll)
                                    .desired_width(ui.available_width()),
                            )
                            .changed()
                        {
                            self.clear_snapshot();
                            self.pin.clear();
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.label("AID (hex, optional)");
                        if ui.text_edit_singleline(&mut self.aid).changed() {
                            self.clear_snapshot();
                        }
                    });
                    let mut show_pan = self.show_pan;
                    ui.checkbox(&mut show_pan, "Show full PAN");
                    self.set_show_pan(show_pan);
                    if self.show_pan {
                        ui.colored_label(
                            egui::Color32::RED,
                            "Full PAN enabled. Keep the display private.",
                        );
                    }
                    ui.small("GPO may affect card counters; no PIN or payment authorization.");
                }
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
                        self.clear_snapshot();
                        self.pin.clear();
                    }
                    let read_label = if self.mode == CardMode::Emv {
                        "Read EMV"
                    } else {
                        "Read memory"
                    };
                    if ui
                        .add_enabled(
                            self.mode == CardMode::Emv || cfg!(windows),
                            egui::Button::new(read_label),
                        )
                        .clicked()
                    {
                        match self.read_operation() {
                            Ok(operation) => self.start(operation, ctx),
                            Err(error) => self.error = Some(error.to_string()),
                        }
                    }
                });
                if self.mode == CardMode::Sle4428 {
                    ui.add_enabled_ui(cfg!(windows), |ui| {
                        ui.separator();
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Write address (hex)");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.address).desired_width(80.0),
                            );
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
                                    egui::TextEdit::singleline(&mut self.field_length)
                                        .desired_width(80.0),
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
                                egui::TextEdit::singleline(&mut self.preview_address)
                                    .desired_width(70.0),
                            );
                            ui.label("Preview length (bytes)");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.preview_length)
                                    .desired_width(70.0),
                            );
                        });
                    });
                }
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
            if self.mode == CardMode::Sle4428 && !cfg!(windows) {
                ui.colored_label(
                    egui::Color32::RED,
                    "SLE4428 access is supported only on Windows.",
                );
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(report) = &self.emv {
                ui.heading("EMV information");
                egui::ScrollArea::both()
                    .id_salt("emv-information")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(report).monospace())
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
            }
            if let Some(memory) = &self.memory {
                ui.columns(2, |columns| {
                    columns[0].heading("Read snapshot");
                    egui::ScrollArea::both()
                        .id_salt("snapshot-hex")
                        .scroll_source(egui::scroll_area::ScrollSource {
                            drag: false,
                            ..Default::default()
                        })
                        .auto_shrink([false, false])
                        .show(&mut columns[0], |ui| {
                            snapshot_view(
                                ui,
                                "snapshot-hex-selection",
                                &SnapshotText::hex(memory.as_slice()),
                                &mut self.snapshot_selection,
                                false,
                            );
                        });
                    columns[1].heading("Readable string");
                    egui::ScrollArea::vertical()
                        .id_salt("snapshot-text")
                        .scroll_source(egui::scroll_area::ScrollSource {
                            drag: false,
                            ..Default::default()
                        })
                        .auto_shrink([false, false])
                        .show(&mut columns[1], |ui| {
                            match text_preview(
                                memory.as_slice(),
                                &self.preview_address,
                                &self.preview_length,
                            ) {
                                Ok((address, text)) => {
                                    snapshot_view(
                                        ui,
                                        ("snapshot-text-selection", address, text.len()),
                                        &SnapshotText::readable(address, text),
                                        &mut self.snapshot_selection,
                                        true,
                                    );
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

    fn synthetic_emv_report(show_pan: bool) -> String {
        let mut responses = vec![
            vec![
                0x6F, 12, 0x5A, 4, 0x12, 0x34, 0x56, 0x78, 0x57, 4, 0x11, 0x22, 0x33, 0x44, 0x90, 0,
            ],
            vec![0x80, 2, 0, 0, 0x90, 0],
        ]
        .into_iter();
        let report = emv_report(
            |_| Ok(responses.next().unwrap()),
            Some(&[0xA0, 0, 0, 0, 3]),
            show_pan,
        )
        .unwrap();
        assert!(responses.next().is_none());
        report
    }

    #[test]
    fn full_pan_is_opt_in_and_visibility_changes_discard_results() {
        let mut app = CardApp {
            mode: CardMode::Emv,
            readers: vec![CString::new("Synthetic reader").unwrap()],
            selected: Some(0),
            emv: Some(synthetic_emv_report(false)),
            ..Default::default()
        };
        assert!(!app.show_pan);
        assert!(matches!(
            app.read_operation().unwrap(),
            Operation::ReadEmv {
                show_pan: false,
                ..
            }
        ));
        app.set_show_pan(true);
        assert!(app.emv.is_none());
        assert!(app.status.contains("Read EMV"));
        assert!(matches!(
            app.read_operation().unwrap(),
            Operation::ReadEmv { show_pan: true, .. }
        ));
        let report = synthetic_emv_report(true);
        assert!(report.contains("Card number (full)"));
        assert!(report.contains("12345678"));
        assert!(!report.contains("11223344"));
        app.emv = Some(report);
        app.set_show_pan(false);
        assert!(app.emv.is_none());
        assert!(!app.show_pan);
        app.set_show_pan(true);
        app.change_mode(CardMode::Sle4428);
        assert!(!app.show_pan);
        app.change_mode(CardMode::Emv);
        assert!(!app.show_pan);
    }

    #[test]
    fn full_pan_report_handles_missing_pan_and_propagates_errors() {
        let mut responses = vec![vec![0x6F, 0, 0x90, 0], vec![0x80, 2, 0, 0, 0x90, 0]].into_iter();
        let report = emv_report(
            |_| Ok(responses.next().unwrap()),
            Some(&[0xA0, 0, 0, 0, 3]),
            true,
        )
        .unwrap();
        let row = report
            .lines()
            .find(|line| line.contains("Card number (full)"))
            .unwrap();
        assert!(row.contains("Not provided"));
        assert!(
            emv_report(|_| Err(io::Error::other("Synthetic error")), None, true)
                .unwrap_err()
                .to_string()
                .contains("Synthetic error")
        );
    }

    #[test]
    fn emv_mode_needs_no_dll_and_rejects_writes_and_invalid_aids() {
        let mut app = CardApp {
            mode: CardMode::Emv,
            readers: vec![CString::new("Synthetic reader").unwrap()],
            selected: Some(0),
            dll: String::new(),
            ..Default::default()
        };
        assert!(matches!(
            app.read_operation().unwrap(),
            Operation::ReadEmv { aid: None, .. }
        ));
        app.aid = "A000000003".to_owned();
        match app.read_operation().unwrap() {
            Operation::ReadEmv { aid, reader, .. } => {
                assert_eq!(aid.unwrap(), [0xA0, 0, 0, 0, 3]);
                assert_eq!(reader, app.readers[0]);
            }
            _ => panic!("Expected an EMV operation"),
        }
        assert_eq!(
            app.review().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(app.pending.is_none());
        app.aid = "invalid".to_owned();
        assert!(app.read_operation().is_err());
        app.selected = None;
        assert!(app.read_operation().is_err());
    }

    #[test]
    fn switching_modes_clears_snapshots_and_pin() {
        let mut app = CardApp {
            memory: Some(Box::new([0; MEMORY_SIZE])),
            pin: "1234".to_owned(),
            ..Default::default()
        };
        app.change_mode(CardMode::Emv);
        assert!(app.memory.is_none());
        assert!(app.pin.is_empty());
        app.emv = Some("Synthetic information".to_owned());
        app.change_mode(CardMode::Sle4428);
        assert!(app.emv.is_none());
    }

    #[test]
    fn emv_worker_output_is_masked_and_errors_clear_it() {
        let report = synthetic_emv_report(false);
        assert!(report.contains("Card number (masked)"));
        assert!(report.contains("5678"));
        assert!(!report.contains("12345678"));
        let (sender, receiver) = mpsc::channel();
        let mut app = CardApp {
            mode: CardMode::Emv,
            worker: Some(receiver),
            ..Default::default()
        };
        sender.send(Ok(Output::Emv(report))).unwrap();
        app.poll();
        assert!(app.emv.is_some());
        assert!(app.worker.is_none());
        let (sender, receiver) = mpsc::channel();
        app.worker = Some(receiver);
        sender
            .send(Err(io::Error::other("Synthetic EMV failure")))
            .unwrap();
        app.poll();
        assert!(app.emv.is_none());
        assert!(app.error.unwrap().contains("Synthetic EMV failure"));
    }

    #[test]
    fn emv_ui_shows_information_without_memory_write_controls() {
        let mut app = CardApp {
            mode: CardMode::Emv,
            emv: Some("Synthetic EMV information".to_owned()),
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
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some(text.galley.job.text.as_str())
                } else {
                    None
                }
            })
            .collect();
        for label in [
            "AID (hex, optional)",
            "Show full PAN",
            "Read EMV",
            "EMV information",
            "Synthetic EMV information",
        ] {
            assert!(labels.contains(&label), "Missing {label}");
        }
        for label in [
            "HID DLL",
            "PIN (hex)",
            "Review write...",
            "Preview address (hex)",
        ] {
            assert!(!labels.contains(&label), "Unexpected {label}");
        }
        assert!(app.worker.is_none());
    }

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
        assert_eq!(text_preview(&memory, "0", "2").unwrap(), (0, "Hi"));
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
    fn snapshot_selection_maps_hex_digits_and_utf8_to_byte_addresses() {
        let hex = SnapshotText::hex(&[0x41; 32]);
        assert_eq!(hex.selected_bytes(0..6), None);
        assert_eq!(hex.selected_bytes(6..7), Some(0..1));
        assert_eq!(hex.selected_bytes(7..8), Some(0..1));
        assert_eq!(hex.selected_bytes(8..9), None);
        assert_eq!(hex.selected_bytes(7..11), Some(0..2));
        let next_row = hex.text.find('\n').unwrap() + 1;
        assert_eq!(hex.selected_bytes(51..next_row + 8), Some(15..17));
        assert_eq!(hex.selected_bytes(6..6), None);

        let text = SnapshotText::readable(0x20, "A\u{20AC}\n\u{1F600}Z");
        assert_eq!(text.selected_bytes(1..2), Some(0x21..0x24));
        assert_eq!(text.selected_bytes(2..4), Some(0x24..0x29));
        assert_eq!(text.selected_bytes(0..5), Some(0x20..0x2A));
        assert_eq!(text.selected_bytes(3..3), None);
    }

    #[test]
    fn snapshot_layout_highlights_corresponding_bytes_in_both_views() {
        let selection = 1..4;
        let highlight = egui::Color32::BLUE;
        let text = SnapshotText::readable(0, "A\u{20AC}Z");
        let hex = SnapshotText::hex("A\u{20AC}Z".as_bytes());
        let highlighted = |snapshot: &SnapshotText| {
            let job = snapshot.layout(
                egui::FontId::monospace(14.0),
                egui::Color32::WHITE,
                highlight,
                Some(&selection),
                f32::INFINITY,
            );
            job.sections
                .iter()
                .filter(|section| section.format.background == highlight)
                .map(|section| &job.text[section.byte_range.clone()])
                .collect::<String>()
        };
        assert_eq!(highlighted(&text), "\u{20AC}");
        assert_eq!(highlighted(&hex), "E282AC");
    }

    #[test]
    fn clearing_snapshot_discards_linked_selection() {
        let mut app = CardApp {
            memory: Some(Box::new([0; MEMORY_SIZE])),
            snapshot_selection: Some(0x20..0x25),
            ..Default::default()
        };
        app.clear_snapshot();
        assert!(app.snapshot_selection.is_none());
    }

    #[test]
    fn snapshot_mouse_selection_links_both_directions_and_supports_copy() {
        fn render(
            ctx: &egui::Context,
            snapshots: &[SnapshotText; 2],
            selection: &mut Option<Range<usize>>,
            events: Vec<egui::Event>,
        ) -> (egui::FullOutput, Vec<egui::text_edit::TextEditOutput>) {
            let mut views = Vec::new();
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.columns(2, |columns| {
                            for index in 0..2 {
                                let scroll = if index == 0 {
                                    egui::ScrollArea::both()
                                } else {
                                    egui::ScrollArea::vertical()
                                };
                                views.push(
                                    scroll
                                        .id_salt(index)
                                        .scroll_source(egui::scroll_area::ScrollSource {
                                            drag: false,
                                            ..Default::default()
                                        })
                                        .show(&mut columns[index], |ui| {
                                            snapshot_view(
                                                ui,
                                                index,
                                                &snapshots[index],
                                                selection,
                                                index == 1,
                                            )
                                        })
                                        .inner,
                                );
                            }
                        });
                    });
                },
            );
            (output, views)
        }

        for (source, characters, expected_copy) in [(0, 9..17, "E2 82 AC"), (1, 1..2, "\u{20AC}")] {
            let ctx = egui::Context::default();
            let snapshots = [
                SnapshotText::hex("A\u{20AC}Z".as_bytes()),
                SnapshotText::readable(0, "A\u{20AC}Z"),
            ];
            let mut selection = None;
            render(&ctx, &snapshots, &mut selection, Vec::new());
            let (_, views) = render(&ctx, &snapshots, &mut selection, Vec::new());
            assert!(views.iter().all(|view| view.response.rect.is_finite()));
            let position = |index| {
                views[source].galley_pos
                    + views[source]
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(index))
                        .center()
                        .to_vec2()
            };
            let start = position(characters.start);
            let end = position(characters.end);
            render(
                &ctx,
                &snapshots,
                &mut selection,
                vec![
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            render(
                &ctx,
                &snapshots,
                &mut selection,
                vec![egui::Event::PointerMoved(end)],
            );
            render(
                &ctx,
                &snapshots,
                &mut selection,
                vec![egui::Event::PointerButton {
                    pos: end,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            );
            assert_eq!(selection, Some(1..4), "Source panel {source}");
            let (output, views) = render(&ctx, &snapshots, &mut selection, vec![egui::Event::Copy]);
            assert!(output.platform_output.commands.iter().any(|command| {
                matches!(command, egui::OutputCommand::CopyText(text) if text == expected_copy)
            }));
            for (index, view) in views.iter().enumerate() {
                assert!(
                    view.galley.job.sections.iter().any(|section| {
                        section.format.background == ctx.style().visuals.selection.bg_fill
                    }),
                    "Source {source}, panel {index}, selection {selection:?}, cursor {:?}",
                    view.cursor_range
                );
            }
            let (_, views) = render(
                &ctx,
                &snapshots,
                &mut selection,
                vec![egui::Event::Text("overwrite".to_owned())],
            );
            assert_eq!(views[0].galley.job.text, snapshots[0].text);
            assert_eq!(views[1].galley.job.text, snapshots[1].text);
        }

        for (index, address, expected_copy) in [
            (6, 0, "41"),
            (7, 0, "41"),
            (9, 1, "E2"),
            (10, 1, "E2"),
            (12, 2, "82"),
            (13, 2, "82"),
            (15, 3, "AC"),
            (16, 3, "AC"),
            (18, 4, "5A"),
            (19, 4, "5A"),
        ] {
            for fraction in [0.25, 0.75] {
                let ctx = egui::Context::default();
                let snapshots = [
                    SnapshotText::hex("A\u{20AC}Z".as_bytes()),
                    SnapshotText::readable(0, "A\u{20AC}Z"),
                ];
                let mut selection = None;
                render(&ctx, &snapshots, &mut selection, Vec::new());
                let (_, views) = render(&ctx, &snapshots, &mut selection, Vec::new());
                let start = views[0]
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(index))
                    .center();
                let end = views[0]
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(index + 1))
                    .center();
                let position = views[0].galley_pos + start.lerp(end, fraction).to_vec2();
                for pressed in [true, false] {
                    render(
                        &ctx,
                        &snapshots,
                        &mut selection,
                        vec![
                            egui::Event::PointerMoved(position),
                            egui::Event::PointerButton {
                                pos: position,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: Default::default(),
                            },
                        ],
                    );
                }
                assert_eq!(
                    selection,
                    Some(address..address + 1),
                    "Digit {index}, fraction {fraction}"
                );
                let (output, views) =
                    render(&ctx, &snapshots, &mut selection, vec![egui::Event::Copy]);
                assert!(output.platform_output.commands.iter().any(|command| {
                    matches!(command, egui::OutputCommand::CopyText(text) if text == expected_copy)
                }));
                let highlighted_text = views[1]
                    .galley
                    .job
                    .sections
                    .iter()
                    .filter(|section| {
                        section.format.background == ctx.style().visuals.selection.bg_fill
                    })
                    .map(|section| &views[1].galley.job.text[section.byte_range.clone()])
                    .collect::<String>();
                assert_eq!(
                    highlighted_text,
                    match address {
                        0 => "A",
                        4 => "Z",
                        _ => "\u{20AC}",
                    }
                );
                for index in [0, 4, 5, 8, 11, 14, 17] {
                    let start = views[0]
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(index))
                        .center();
                    let end = views[0]
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(index + 1))
                        .center();
                    assert!(
                        snapshots[0]
                            .clicked_hex_characters(
                                &views[0].galley,
                                start.lerp(end, 0.5).to_vec2()
                            )
                            .is_none()
                    );
                }
            }
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
