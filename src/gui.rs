use crate::{
    discovery,
    model::{Config, Entry, ManagedApp, Origin, RestartPolicy, Snapshot, SupervisorStatus, now},
    platform::{self, Action},
    store::Store,
    supervisor,
};
use anyhow::Result;
use eframe::egui::{self, Color32 as C, RichText as R};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

const NAVY: C = C::from_rgb(20, 31, 48);
const INK: C = C::from_rgb(29, 43, 61);
const MUTED: C = C::from_rgb(111, 126, 143);
const TEAL: C = C::from_rgb(15, 126, 113);
const LINE: C = C::from_rgb(225, 231, 235);
const BG: C = C::from_rgb(246, 248, 250);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Apps,
    Managed,
    Startup,
    Schedules,
    Processes,
    Diagnostics,
    Settings,
}
impl Page {
    fn label(self) -> &'static str {
        match self {
            Self::Apps => "All applications",
            Self::Managed => "Managed apps",
            Self::Startup => "Startup",
            Self::Schedules => "Schedules & services",
            Self::Processes => "Running processes",
            Self::Diagnostics => "Discovery report",
            Self::Settings => "Settings",
        }
    }
    fn subtitle(self) -> &'static str {
        match self {
            Self::Apps => "Installed tools, startup entries, and background activity in one place.",
            Self::Managed => "Your applications, with predictable recovery and quiet restarts.",
            Self::Startup => "See what is enabled at login or boot, and who manages it.",
            Self::Schedules => {
                "Existing jobs and services, controlled through their native manager."
            }
            Self::Processes => "Live process observations from the last scan.",
            Self::Diagnostics => "What AService found, and where discovery has limits.",
            Self::Settings => "Background supervision and machine integration.",
        }
    }
}
enum Event {
    Scanned(Snapshot, Result<bool>),
    Done(Result<String>),
}
enum Confirm {
    External(Box<Entry>, Action),
    Remove(String),
    Startup(bool),
}

struct Editor {
    app: ManagedApp,
    args: String,
    env: String,
    dir: String,
    error: String,
    existing: bool,
}
impl Editor {
    fn new(app: ManagedApp, existing: bool) -> Self {
        Self {
            args: app.args.join("\n"),
            env: serde_json::to_string_pretty(&app.env).unwrap_or("{}".into()),
            dir: app
                .working_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            app,
            error: String::new(),
            existing,
        }
    }
}

pub struct Dashboard {
    store: Store,
    _gui_lock: File,
    page: Page,
    snapshot: Snapshot,
    config: Config,
    status: SupervisorStatus,
    selected: Option<String>,
    query: String,
    runtime: String,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    scanning: bool,
    busy: bool,
    message: Option<(bool, String)>,
    editor: Option<Editor>,
    confirm: Option<Confirm>,
    last_poll: Instant,
    last_scan: Instant,
    startup: Option<bool>,
    log: String,
    show_logs: bool,
    status_error: Option<String>,
}

pub fn run(store: Store) -> Result<()> {
    let gui_lock = store
        .lock("gui.lock", true)
        .map_err(|_| anyhow::anyhow!("AService is already open for this profile"))?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([1050.0, 680.0])
            .with_title("AService — Background applications"),
        ..Default::default()
    };
    eframe::run_native(
        "AService",
        options,
        Box::new(move |cc| {
            let mut style = egui::Style {
                visuals: egui::Visuals::light(),
                ..Default::default()
            };
            style.visuals.panel_fill = BG;
            style.visuals.window_fill = C::WHITE;
            style.visuals.override_text_color = Some(INK);
            style.visuals.selection.bg_fill = C::from_rgb(215, 239, 232);
            style.visuals.selection.stroke = egui::Stroke::new(1.0, TEAL);
            style.spacing.item_spacing = egui::vec2(10.0, 10.0);
            style.spacing.button_padding = egui::vec2(12.0, 9.0);
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
            cc.egui_ctx.set_theme(egui::Theme::Light);
            cc.egui_ctx.set_style_of(egui::Theme::Light, style);
            let (tx, rx) = mpsc::channel();
            let mut app = Dashboard {
                store,
                _gui_lock: gui_lock,
                page: Page::Apps,
                snapshot: Snapshot::default(),
                config: Config::default(),
                status: SupervisorStatus::default(),
                selected: None,
                query: String::new(),
                runtime: "All runtimes".into(),
                tx,
                rx,
                scanning: false,
                busy: false,
                message: None,
                editor: None,
                confirm: None,
                last_poll: Instant::now() - Duration::from_secs(5),
                last_scan: Instant::now(),
                startup: None,
                log: String::new(),
                show_logs: false,
                status_error: None,
            };
            app.refresh();
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
}

impl Dashboard {
    fn refresh(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let store = self.store.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let snapshot = discovery::scan(&store);
            let startup = platform::startup_enabled(&store);
            let _ = tx.send(Event::Scanned(snapshot, startup));
        });
        self.last_scan = Instant::now();
    }
    fn action(&mut self, f: impl FnOnce(Store) -> Result<String> + Send + 'static) {
        if self.busy {
            return;
        }
        self.busy = true;
        let store = self.store.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Event::Done(f(store)));
        });
    }
    fn managed_intent(&mut self, id: String, running: bool, restart: bool) {
        self.action(move |store| {
            store.update(|c| {
                let app = c
                    .apps
                    .iter_mut()
                    .find(|a| a.id == id)
                    .ok_or_else(|| anyhow::anyhow!("App no longer exists"))?;
                app.desired_running = running;
                if running || restart {
                    app.generation = app.generation.wrapping_add(1);
                }
                Ok(())
            })?;
            supervisor::ensure_running(&store)?;
            Ok(if running {
                "Start requested"
            } else {
                "Stop requested; automatic restart suppressed"
            }
            .into())
        });
    }
    fn poll(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Scanned(snapshot, startup) => {
                    self.snapshot = snapshot;
                    self.scanning = false;
                    match startup {
                        Ok(on) => self.startup = Some(on),
                        Err(e) => {
                            self.startup = None;
                            self.snapshot
                                .warnings
                                .push(format!("Supervisor login registration: {e:#}"));
                        }
                    }
                }
                Event::Done(result) => {
                    self.busy = false;
                    self.message = Some(match result {
                        Ok(s) => (true, s),
                        Err(e) => (false, format!("{e:#}")),
                    });
                    self.refresh();
                    self.last_poll = Instant::now() - Duration::from_secs(2);
                }
            }
        }
        if self.last_poll.elapsed() > Duration::from_secs(1) {
            match self.store.load() {
                Ok(c) => {
                    self.config = c;
                    self.status_error = None;
                }
                Err(e) => self.status_error = Some(format!("{e:#}")),
            }
            match self.store.status() {
                Ok(s) => self.status = s,
                Err(e) => self.status_error = Some(format!("Status: {e:#}")),
            }
            self.last_poll = Instant::now();
            if self.show_logs {
                self.read_log();
            }
        }
    }
    fn supervisor_alive(&self) -> bool {
        now().saturating_sub(self.status.updated_at) < 5 && self.store.is_locked("daemon.lock")
    }
    fn entries(&self) -> Vec<Entry> {
        let mut entries = self
            .snapshot
            .entries
            .iter()
            .filter(|e| e.origin != Origin::Managed)
            .cloned()
            .collect::<Vec<_>>();
        for app in &self.config.apps {
            let mut e = Entry::new(
                app.id.clone(),
                app.name.clone(),
                Origin::Managed,
                format!("{} {}", app.program.display(), app.args.join(" ")),
            );
            e.program = Some(app.program.clone());
            e.args = app.args.clone();
            e.startup = Some(app.start_on_login);
            e.location = app
                .working_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            if self.supervisor_alive() {
                if let Some(s) = self.status.apps.get(&app.id) {
                    e.running = Some(s.pid.is_some());
                    e.pid = s.pid;
                    e.detail = s.message.clone();
                }
            } else {
                e.detail = "Supervisor is offline. Start an app to launch it.".into();
            }
            entries.push(e);
        }
        entries
    }
    fn matches(&self, e: &Entry) -> bool {
        let source = match self.page {
            Page::Apps => e.origin != Origin::Process,
            Page::Managed => e.origin == Origin::Managed,
            Page::Startup => {
                e.startup.is_some()
                    && (!matches!(e.origin, Origin::WindowsTask { .. } | Origin::Cron { .. })
                        || e.detail.contains("LogonTrigger")
                        || e.detail.contains("BootTrigger")
                        || e.command.starts_with("@reboot"))
            }
            Page::Schedules => matches!(
                e.origin,
                Origin::WindowsTask { .. }
                    | Origin::WindowsService { .. }
                    | Origin::Systemd { .. }
                    | Origin::Launchd { .. }
                    | Origin::Cron { .. }
                    | Origin::BackgroundConfig { .. }
            ),
            Page::Processes => e.origin == Origin::Process,
            _ => false,
        };
        let q = self.query.to_lowercase();
        source
            && (self.runtime == "All runtimes" || e.runtime == self.runtime)
            && (q.is_empty()
                || format!(
                    "{} {} {} {}",
                    e.name,
                    e.command,
                    e.origin.label(),
                    e.location
                )
                .to_lowercase()
                .contains(&q))
    }
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("navigation")
            .exact_size(204.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(NAVY).inner_margin(20))
            .show(ui, |ui| {
                ui.visuals_mut().override_text_color = Some(C::from_rgb(199, 210, 224));
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.label(
                        R::new("a/")
                            .size(30.0)
                            .color(C::from_rgb(93, 220, 183))
                            .strong(),
                    );
                    ui.label(R::new("AService").size(24.0).strong().color(C::WHITE));
                });
                ui.label(
                    R::new("BACKGROUND, HANDLED.")
                        .size(9.0)
                        .color(C::from_rgb(126, 148, 169)),
                );
                ui.add_space(38.0);
                for page in [
                    Page::Apps,
                    Page::Managed,
                    Page::Startup,
                    Page::Schedules,
                    Page::Processes,
                ] {
                    self.nav_item(ui, page);
                }
                ui.add_space(28.0);
                ui.label(
                    R::new("WORKSPACE")
                        .size(10.0)
                        .color(C::from_rgb(119, 142, 165)),
                );
                ui.add_space(4.0);
                self.nav_item(ui, Page::Diagnostics);
                self.nav_item(ui, Page::Settings);
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        R::new(format!(
                            "v{}  ·  {}",
                            env!("CARGO_PKG_VERSION"),
                            std::env::consts::OS
                        ))
                        .size(11.0)
                        .color(MUTED),
                    );
                    ui.label(
                        R::new("Local control. Quiet recovery.")
                            .size(11.0)
                            .color(C::from_rgb(146, 168, 188)),
                    );
                    let alive = self.supervisor_alive();
                    ui.label(
                        R::new(if alive {
                            "●  Supervisor online"
                        } else {
                            "○  Supervisor idle"
                        })
                        .size(12.0)
                        .color(if alive {
                            C::from_rgb(93, 220, 183)
                        } else {
                            C::from_rgb(146, 168, 188)
                        }),
                    );
                });
            });
    }
    fn nav_item(&mut self, ui: &mut egui::Ui, page: Page) {
        let selected = self.page == page;
        let button = egui::Button::new(R::new(page.label()).size(13.0).color(if selected {
            C::WHITE
        } else {
            C::from_rgb(177, 193, 210)
        }))
        .fill(if selected {
            C::from_rgb(42, 61, 79)
        } else {
            C::TRANSPARENT
        })
        .min_size(egui::vec2(164.0, 39.0));
        if ui.add(button).clicked() {
            self.page = page;
            self.query.clear();
            self.selected = None;
            self.show_logs = false;
        }
    }
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    R::new("YOUR WORKSPACE / APPLICATIONS")
                        .size(10.0)
                        .strong()
                        .color(TEAL),
                );
                ui.add_space(3.0);
                ui.label(R::new(self.page.label()).size(29.0).strong());
                ui.label(R::new(self.page.subtitle()).color(MUTED).size(13.0));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(R::new("+  Add application").color(C::WHITE)).fill(TEAL))
                    .clicked()
                {
                    self.editor = Some(Editor::new(ManagedApp::default(), false));
                }
                if ui
                    .add_enabled(
                        !self.scanning,
                        egui::Button::new(if self.scanning {
                            "Scanning…"
                        } else {
                            "Refresh"
                        }),
                    )
                    .clicked()
                {
                    self.refresh();
                }
            });
        });
        ui.add_space(20.0);
    }
    fn metrics(&self, ui: &mut egui::Ui, entries: &[Entry]) {
        ui.columns(4, |cols| {
            let values = [
                (
                    "INSTALLED TOOLS",
                    entries
                        .iter()
                        .filter(|e| matches!(e.origin, Origin::Installed { .. }))
                        .count(),
                    "Across detected runtimes",
                ),
                (
                    "MANAGED BY YOU",
                    self.config.apps.len(),
                    "Independent of the window",
                ),
                (
                    "ENABLED ENTRIES",
                    entries.iter().filter(|e| e.startup == Some(true)).count(),
                    "Startup, services & schedules",
                ),
                (
                    "LIVE PROCESSES",
                    entries
                        .iter()
                        .filter(|e| e.origin == Origin::Process)
                        .count(),
                    "Observed in the last scan",
                ),
            ];
            for (col, (label, value, note)) in cols.iter_mut().zip(values) {
                card().show(col, |ui| {
                    ui.set_min_height(86.0);
                    ui.label(R::new(label).size(10.0).strong().color(MUTED));
                    ui.label(R::new(value.to_string()).size(28.0).strong());
                    ui.label(R::new(note).size(11.0).color(MUTED));
                });
            }
        });
        ui.add_space(22.0);
    }
    fn list(&mut self, ui: &mut egui::Ui, entries: &[Entry]) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search applications, commands, or sources…")
                    .desired_width(290.0),
            );
            egui::ComboBox::from_id_salt("runtime")
                .selected_text(&self.runtime)
                .width(130.0)
                .show_ui(ui, |ui| {
                    for runtime in [
                        "All runtimes",
                        "Node.js",
                        "Bun",
                        "Go",
                        "Rust",
                        "Python",
                        "Deno",
                        ".NET",
                        "Java",
                        "Native / other",
                    ] {
                        ui.selectable_value(&mut self.runtime, runtime.into(), runtime);
                    }
                });
        });
        ui.add_space(12.0);
        let rows = entries
            .iter()
            .filter(|e| self.matches(e))
            .collect::<Vec<_>>();
        ui.horizontal(|ui| {
            ui.label(
                R::new(format!("{} records", rows.len()))
                    .size(12.0)
                    .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    R::new(if self.scanning {
                        "Scanning sources…"
                    } else {
                        "Select an app to inspect"
                    })
                    .size(11.0)
                    .color(MUTED),
                );
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("applications")
            .auto_shrink([false, false])
            .show_rows(ui, 72.0, rows.len(), |ui, range| {
                for index in range {
                    let e = rows[index];
                    let selected = self.selected.as_deref() == Some(&e.id);
                    let response = egui::Frame::new()
                        .fill(if selected {
                            C::from_rgb(231, 243, 239)
                        } else {
                            C::WHITE
                        })
                        .stroke(egui::Stroke::new(
                            1.0,
                            if selected {
                                C::from_rgb(158, 208, 190)
                            } else {
                                LINE
                            },
                        ))
                        .corner_radius(8)
                        .inner_margin(12)
                        .show(ui, |ui| {
                            ui.set_min_size(egui::vec2(ui.available_width(), 40.0));
                            ui.horizontal(|ui| {
                                egui::Frame::new()
                                    .fill(runtime_color(&e.runtime).gamma_multiply(0.13))
                                    .corner_radius(7)
                                    .inner_margin(8)
                                    .show(ui, |ui| {
                                        ui.label(
                                            R::new(runtime_initial(&e.runtime))
                                                .strong()
                                                .color(runtime_color(&e.runtime))
                                                .size(17.0),
                                        );
                                    });
                                ui.vertical(|ui| {
                                    ui.label(R::new(ellipsize(&e.name, 45)).strong().size(14.0));
                                    ui.label(
                                        R::new(format!("{}   ·   {}", e.runtime, e.origin.label()))
                                            .size(11.0)
                                            .color(MUTED),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let state = if e.origin == Origin::Managed {
                                            self.status
                                                .apps
                                                .get(&e.id)
                                                .filter(|_| self.supervisor_alive())
                                                .map(|s| s.state.as_str())
                                                .unwrap_or("Stopped")
                                        } else if e.running == Some(true) {
                                            "Running"
                                        } else if e.startup == Some(true) {
                                            "Enabled"
                                        } else if e.startup == Some(false) {
                                            "Disabled"
                                        } else {
                                            "Installed"
                                        };
                                        ui.label(R::new(state).size(11.0).color(
                                            if matches!(state, "Running" | "Enabled") {
                                                TEAL
                                            } else {
                                                MUTED
                                            },
                                        ));
                                    },
                                );
                            });
                        })
                        .response;
                    if ui
                        .interact(response.rect, response.id.with(&e.id), egui::Sense::click())
                        .clicked()
                    {
                        self.selected = Some(e.id.clone());
                        self.show_logs = false;
                        self.log.clear();
                    }
                    ui.add_space(6.0);
                }
            });
        if rows.is_empty() {
            ui.add_space(45.0);
            ui.centered_and_justified(|ui| {
                ui.label(
                    R::new(if self.scanning {
                        "Discovering applications…"
                    } else {
                        "No matching applications. Refresh or add your first managed app."
                    })
                    .color(MUTED),
                );
            });
        }
    }
    fn detail(&mut self, ui: &mut egui::Ui, entries: &[Entry]) {
        let entry = self
            .selected
            .as_ref()
            .and_then(|id| entries.iter().find(|e| &e.id == id))
            .cloned();
        if let Some(entry) = entry {
            egui::Panel::right("details").exact_size(320.0).resizable(false).frame(egui::Frame::new().fill(C::WHITE).inner_margin(20).stroke(egui::Stroke::new(1.0,LINE))).show(ui,|ui|{
                egui::ScrollArea::vertical().id_salt("details-scroll").show(ui,|ui|{
                    ui.horizontal(|ui|{ui.label(R::new("APPLICATION DETAILS").size(10.0).color(MUTED).strong());ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{if ui.small_button("Close").clicked(){self.selected=None;}});});
                    ui.add_space(12.0);ui.label(R::new(&entry.name).size(21.0).strong());ui.label(R::new(format!("{}  ·  {}",entry.runtime,entry.origin.label())).size(12.0).color(TEAL));ui.add_space(16.0);
                    label(ui,"COMMAND");ui.add(egui::Label::new(R::new(&entry.command).monospace().size(11.0)).wrap());
                    if !entry.location.is_empty(){ui.add_space(10.0);label(ui,"SOURCE / WORKING DIRECTORY");ui.add(egui::Label::new(R::new(&entry.location).size(11.0).color(MUTED)).wrap());}
                    if let Some(pid)=entry.pid{ui.label(R::new(format!("PID {pid}")).monospace().size(11.0));}
                    ui.add_space(16.0);ui.separator();ui.add_space(8.0);
                    ui.add_enabled_ui(!self.busy,|ui|{
                        if entry.origin==Origin::Managed{self.managed_detail(ui,&entry);}else{
                            if entry.origin.can_toggle(){ui.horizontal(|ui|{ui.label(if matches!(entry.origin,Origin::WindowsTask{..}){"Schedule enabled"}else{"Auto-start enabled"});let mut on=entry.startup.unwrap_or(false);if ui.checkbox(&mut on,"").changed(){self.confirm=Some(Confirm::External(Box::new(entry.clone()),if on{Action::Enable}else{Action::Disable}));}});}
                            if entry.origin.can_run(){ui.horizontal(|ui|{if ui.button("Start").clicked(){self.confirm=Some(Confirm::External(Box::new(entry.clone()),Action::Start));}
                            if ui.button("Stop").clicked(){self.confirm=Some(Confirm::External(Box::new(entry.clone()),Action::Stop));}});}
                            ui.add_space(8.0);if ui.button("Create managed copy…").clicked(){let app=ManagedApp{name:entry.name.clone(),program:entry.program.clone().unwrap_or_default(),args:entry.args.clone(),working_dir:std::path::Path::new(&entry.location).is_dir().then(||crate::store::command_path(entry.location.clone().into())),..Default::default()};self.editor=Some(Editor::new(app,false));}
                            ui.label(R::new("A managed copy uses separate settings. Review existing startup entries to avoid duplicate launches.").size(11.0).color(MUTED));
                            if !entry.origin.can_toggle()&&!entry.origin.can_run(){ui.label(R::new("This source is available for inspection only.").size(12.0).color(MUTED));}
                        }
                    });
                    ui.add_space(14.0);label(ui,"DETAILS");ui.add(egui::Label::new(R::new(&entry.detail).size(12.0).color(MUTED)).wrap());
                    if self.show_logs{ui.separator();ui.label(R::new("RECENT OUTPUT · LAST 64 KiB").size(10.0).color(MUTED));ui.add(egui::TextEdit::multiline(&mut self.log).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).desired_rows(15).interactive(false));}
                });
            });
        }
    }
    fn managed_detail(&mut self, ui: &mut egui::Ui, entry: &Entry) {
        let Some(app) = self.config.apps.iter().find(|a| a.id == entry.id).cloned() else {
            return;
        };
        let mut login = app.start_on_login;
        let mut background = app.run_in_background;
        if ui
            .checkbox(&mut login, "Start at login")
            .on_hover_text("Enabling also registers the AService supervisor for your next login.")
            .changed()
        {
            let id = app.id.clone();
            self.action(move |store| {
                if login {
                    platform::install_startup(&store, true)?;
                }
                store.update(|c| {
                    if let Some(a) = c.apps.iter_mut().find(|a| a.id == id) {
                        a.start_on_login = login;
                    }
                    Ok(())
                })?;
                Ok("Login preference saved".into())
            });
        }
        if ui
            .checkbox(&mut background, "Keep running after window closes")
            .changed()
        {
            let id = app.id.clone();
            self.action(move |store| {
                store.update(|c| {
                    if let Some(a) = c.apps.iter_mut().find(|a| a.id == id) {
                        a.run_in_background = background;
                    }
                    Ok(())
                })?;
                Ok("Background preference saved".into())
            });
        }
        ui.label(
            R::new(format!("Recovery: {:?}", app.restart))
                .size(12.0)
                .color(MUTED),
        );
        if let Some(s) = self
            .status
            .apps
            .get(&app.id)
            .filter(|_| self.supervisor_alive())
        {
            ui.label(R::new(&s.state).color(TEAL).strong());
            ui.label(
                R::new(format!("Recovery events: {}", s.restarts))
                    .size(11.0)
                    .color(MUTED),
            );
            if let Some(retry) = s.next_retry {
                ui.label(format!("Retry in {}s", retry.saturating_sub(now())));
            }
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Start").clicked() {
                self.managed_intent(app.id.clone(), true, false);
            }
            if ui.button("Stop").clicked() {
                self.managed_intent(app.id.clone(), false, false);
            }
            if ui.button("Restart").clicked() {
                self.managed_intent(app.id.clone(), true, true);
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Edit…").clicked() {
                self.editor = Some(Editor::new(app.clone(), true));
            }
            if ui.button("View logs").clicked() {
                self.show_logs = !self.show_logs;
                self.read_log();
            }
            if ui.button("Remove…").clicked() {
                self.confirm = Some(Confirm::Remove(app.id.clone()));
            }
        });
    }
    fn read_log(&mut self) {
        let Some(id) = &self.selected else {
            return;
        };
        self.log = (|| -> Result<String> {
            let mut f = File::open(self.store.log_path(id))?;
            let size = f.metadata()?.len();
            f.seek(SeekFrom::Start(size.saturating_sub(65536)))?;
            let mut bytes = vec![];
            f.take(65536).read_to_end(&mut bytes)?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        })()
        .unwrap_or_else(|_| "No output has been captured yet.".into());
    }
    fn diagnostics(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui,|ui|{card().show(ui,|ui|{ui.label(R::new("Discovery is read-only").size(20.0).strong());ui.label("AService never adopts an app or changes a startup entry during a scan. Each record names the manager that owns it. Installed, enabled, and running are separate states.");ui.add_space(8.0);ui.label(format!("Last scan: {} · {} records · {} notices",self.snapshot.scanned_at,self.snapshot.entries.len(),self.snapshot.warnings.len()));});
            for warning in &self.snapshot.warnings{ui.add_space(8.0);card().show(ui,|ui|{ui.label(R::new(warning).color(MUTED));});}
        });
    }
    fn settings(&mut self, ui: &mut egui::Ui) {
        card().show(ui,|ui|{
            ui.label(R::new("A quieter background").size(21.0).strong());ui.label("The supervisor runs independently of this window. Managed apps start directly with literal arguments and no interactive console.");ui.add_space(12.0);
            let mut startup=self.startup.unwrap_or(false);if ui.add_enabled(!self.busy&&self.startup.is_some(),egui::Checkbox::new(&mut startup,"Launch AService supervisor at login")).changed(){self.confirm=Some(Confirm::Startup(startup));}
            if self.startup==Some(false)&&self.config.apps.iter().any(|a|a.start_on_login){ui.colored_label(C::from_rgb(169,101,37),"Some apps request startup, but the login helper is disabled. Enable it to run them at login.");}
            ui.add_space(12.0);label(ui,"DEFAULT RECOVERY");ui.label("Restart on failure · 2–60s backoff with jitter · reset after 60s healthy · pause after 5 failures in 5 minutes · 10s stop grace · process-tree cleanup.");ui.label("Clean exits stay stopped by default. An explicit Stop suppresses recovery. Logs rotate at 2 MiB with three backups per app.");
            ui.add_space(12.0);label(ui,"LOCAL DATA");ui.label(R::new(self.store.root.display().to_string()).monospace().size(12.0));ui.label("Configuration, state, logs, and pre-change source backups are stored here. Environment values are local plaintext; prefer OS credentials over storing secrets in app settings.");
            ui.add_space(12.0);label(ui,"SOURCE OWNERSHIP");ui.label("Existing scheduled jobs and services retain native recovery policies. Privileged changes may fail with a permission message; AService never elevates itself silently. Cron, system launchd, and unsupported startup formats remain inspect-only.");
        });
    }
    fn editor(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.editor.take() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        egui::Window::new(if editor.existing{"Edit managed application"}else{"Add managed application"}).id(egui::Id::unique("app-editor")).collapsible(false).resizable(true).default_width(590.0).open(&mut open).show(ctx,|ui|{
            egui::ScrollArea::vertical().max_height(650.0).show(ui,|ui|{
                ui.label(if editor.existing {"Saving updates the definition and restarts the app if it is running."}else{"New entries are saved stopped. Use Start after reviewing the executable and arguments."});
                label(ui,"NAME");ui.text_edit_singleline(&mut editor.app.name);
                label(ui,"ABSOLUTE EXECUTABLE PATH");let mut program=editor.app.program.display().to_string();if ui.add(egui::TextEdit::singleline(&mut program).desired_width(f32::INFINITY).hint_text(if cfg!(windows){"C:\\Program Files\\nodejs\\node.exe"}else{"/usr/local/bin/node"})).changed(){editor.app.program=program.into();}
                ui.label(R::new("For npm/Bun scripts choose node or bun, then pass the script path below. Arguments are literal; shell operators and quotes are not parsed.").size(11.0).color(MUTED));
                label(ui,"ARGUMENTS · ONE PER LINE");ui.add(egui::TextEdit::multiline(&mut editor.args).desired_rows(4).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                label(ui,"WORKING DIRECTORY · OPTIONAL");ui.add(egui::TextEdit::singleline(&mut editor.dir).desired_width(f32::INFINITY));
                label(ui,"ENVIRONMENT OVERRIDES · JSON OBJECT");ui.add(egui::TextEdit::multiline(&mut editor.env).desired_rows(3).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                ui.checkbox(&mut editor.app.start_on_login,"Start at login (registers the AService login helper)");ui.checkbox(&mut editor.app.run_in_background,"Keep running when the AService window closes");
                egui::ComboBox::from_label("Recovery policy").selected_text(format!("{:?}",editor.app.restart)).show_ui(ui,|ui|{ui.selectable_value(&mut editor.app.restart,RestartPolicy::OnFailure,"On failure (recommended)");ui.selectable_value(&mut editor.app.restart,RestartPolicy::Always,"Always");ui.selectable_value(&mut editor.app.restart,RestartPolicy::Never,"Never");});
                if !editor.error.is_empty(){ui.colored_label(C::from_rgb(180,58,58),&editor.error);}
                ui.add_space(10.0);if ui.add_enabled(!self.busy,egui::Button::new(R::new("Save application").color(C::WHITE)).fill(TEAL)).clicked(){save=true;}
            });
        });
        if save {
            editor.app.args = if editor.args.is_empty() {
                vec![]
            } else {
                editor
                    .args
                    .split('\n')
                    .map(|s| s.trim_end_matches('\r').into())
                    .collect()
            };
            editor.app.working_dir = if editor.dir.trim().is_empty() {
                None
            } else {
                Some(editor.dir.clone().into())
            };
            let result = serde_json::from_str(&editor.env)
                .map_err(anyhow::Error::from)
                .and_then(|env| {
                    editor.app.env = env;
                    editor.app.validate()
                });
            match result {
                Err(e) => editor.error = e.to_string(),
                Ok(()) => {
                    let app = editor.app.clone();
                    let existing = editor.existing;
                    self.action(move |store| {
                        if app.start_on_login {
                            platform::install_startup(&store, true)?;
                        }
                        store.update(|c| {
                            if existing {
                                let old = c
                                    .apps
                                    .iter_mut()
                                    .find(|a| a.id == app.id)
                                    .ok_or_else(|| anyhow::anyhow!("App no longer exists"))?;
                                let desired = old.desired_running;
                                let generation = old.generation;
                                *old = app;
                                old.desired_running = desired;
                                old.generation = generation.wrapping_add(1);
                            } else {
                                c.apps.push(app);
                            }
                            Ok(())
                        })?;
                        if existing {
                            supervisor::ensure_running(&store)?;
                        }
                        Ok("Application saved".into())
                    });
                    open = false;
                }
            }
        }
        if open {
            self.editor = Some(editor);
        }
    }
    fn confirmation(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Review change").collapsible(false).resizable(false).default_width(440.0).open(&mut open).show(ctx,|ui|{
            match &confirm{
                Confirm::External(e,a)=>{ui.label(R::new(format!("{} {}?",a.label(),e.name)).size(19.0).strong());ui.label(format!("Manager: {}\nSource: {}",e.origin.label(),e.location));ui.label("AService will request this change from the native manager and save a source backup. Other applications may depend on this service or schedule.");if matches!(e.origin,Origin::WindowsService{..}){ui.label("Disabling auto-start changes the service to Manual; it does not stop the running service. Enabling changes it to Automatic.");}ui.add(egui::Label::new(R::new(&e.command).monospace().size(11.0)).wrap());},
                Confirm::Remove(_)=>{ui.label("Remove this managed app? Its process tree will be stopped. Captured logs are retained.");},
                Confirm::Startup(on)=>{ui.label(if *on{"Register the supervisor for your next login? Apps with Start at login enabled will run silently in your user session."}else{"Disable the supervisor login registration? Managed apps will no longer start automatically. Currently running apps keep running."});},
            }
            ui.add_space(10.0);if ui.add_enabled(!self.busy,egui::Button::new("Apply change")).clicked(){apply=true;}
        });
        if apply {
            match confirm {
                Confirm::External(entry, action) => self.action(move |store| {
                    platform::apply(&store, &entry, action)?;
                    Ok(format!("{} requested for {}", action.label(), entry.name))
                }),
                Confirm::Remove(id) => {
                    self.selected = None;
                    self.action(move |store| {
                        store.update(|c| {
                            c.apps.retain(|a| a.id != id);
                            Ok(())
                        })?;
                        Ok("Application removed; supervisor will stop its process tree".into())
                    });
                }
                Confirm::Startup(on) => self.action(move |store| {
                    platform::install_startup(&store, on)?;
                    Ok("Login registration updated".into())
                }),
            }
        } else if open {
            self.confirm = Some(confirm);
        }
    }
}

impl eframe::App for Dashboard {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(500));
        let entries = self.entries();
        self.sidebar(ui);
        if !matches!(self.page, Page::Diagnostics | Page::Settings) {
            self.detail(ui, &entries);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(26))
            .show(ui, |ui| {
                self.header(ui);
                if let Some(error) = &self.status_error {
                    ui.colored_label(C::from_rgb(180, 58, 58), error);
                }
                if let Some((ok, message)) = self.message.clone() {
                    egui::Frame::new()
                        .fill(if ok {
                            C::from_rgb(222, 241, 232)
                        } else {
                            C::from_rgb(255, 233, 229)
                        })
                        .inner_margin(10)
                        .corner_radius(6)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(message);
                                if ui.small_button("Dismiss").clicked() {
                                    self.message = None;
                                }
                            });
                        });
                    ui.add_space(10.0);
                }
                match self.page {
                    Page::Diagnostics => self.diagnostics(ui),
                    Page::Settings => self.settings(ui),
                    _ => {
                        if self.selected.is_none() {
                            if !self.snapshot.warnings.is_empty() {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        R::new(format!(
                                            "{} discovery notices",
                                            self.snapshot.warnings.len()
                                        ))
                                        .size(11.0)
                                        .color(MUTED),
                                    );
                                    if ui.small_button("View coverage").clicked() {
                                        self.page = Page::Diagnostics;
                                        self.selected = None;
                                    }
                                    ui.label(
                                        R::new(format!(
                                            "Scanned {}s ago",
                                            now().saturating_sub(self.snapshot.scanned_at)
                                        ))
                                        .size(11.0)
                                        .color(MUTED),
                                    );
                                });
                                ui.add_space(10.0);
                            }
                            self.metrics(ui, &entries);
                        }
                        self.list(ui, &entries);
                    }
                }
            });
        self.editor(&ctx);
        self.confirmation(&ctx);
    }
}
fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(C::WHITE)
        .stroke(egui::Stroke::new(1.0, LINE))
        .corner_radius(10)
        .inner_margin(16)
}
fn label(ui: &mut egui::Ui, s: &str) {
    ui.label(R::new(s).size(10.0).strong().color(MUTED));
}
fn runtime_color(s: &str) -> C {
    match s {
        "Node.js" => C::from_rgb(58, 131, 85),
        "Bun" => C::from_rgb(155, 112, 67),
        "Rust" => C::from_rgb(163, 83, 56),
        "Go" => C::from_rgb(25, 138, 161),
        "Python" => C::from_rgb(72, 116, 166),
        _ => C::from_rgb(107, 113, 151),
    }
}
fn runtime_initial(s: &str) -> &str {
    match s {
        "Node.js" => "N",
        "Bun" => "B",
        "Rust" => "R",
        "Go" => "G",
        "Python" => "Py",
        "Deno" => "D",
        ".NET" => ".N",
        _ => "•",
    }
}
fn ellipsize(s: &str, limit: usize) -> String {
    if s.chars().count() > limit {
        format!("{}…", s.chars().take(limit).collect::<String>())
    } else {
        s.into()
    }
}
