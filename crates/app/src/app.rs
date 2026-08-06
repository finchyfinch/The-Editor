//! The application shell.
//!
//! At M0 this is the window frame and nothing more: the dock layout that M1
//! will fill with the file tree, tab bar, editor view, output panel and status
//! bar. The panel structure is here already so that M1 is a matter of putting
//! widgets into slots rather than restructuring the window.

use editor_config::paths::AppPaths;
use eframe::egui;

/// Metadata shown in the About dialog.
pub(crate) struct BuildInfo {
    pub(crate) version: &'static str,
    pub(crate) commit: &'static str,
    pub(crate) date: &'static str,
    pub(crate) rustc: &'static str,
}

pub(crate) const BUILD: BuildInfo = BuildInfo {
    version: env!("CARGO_PKG_VERSION"),
    commit: env!("BUILD_COMMIT"),
    date: env!("BUILD_DATE"),
    rustc: env!("BUILD_RUSTC"),
};

pub(crate) struct EditorApp {
    paths: AppPaths,
    log_dir: String,
    show_about: bool,
    // M1: documents, tabs, file tree, command registry, session.
}

impl EditorApp {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>, paths: AppPaths, log_dir: String) -> Self {
        // Slightly roomier than egui's default, which is tuned for tool panels
        // rather than an application people stare at all day. Applied to both
        // themes so the light/dark toggle in M8 doesn't change the metrics.
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(8.0, 4.0);
        });

        Self {
            paths,
            log_dir,
            show_about: false,
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    ui.label("M1");
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| ui.label("M2"));
                ui.menu_button("View", |ui| ui.label("M1"));
                ui.menu_button("Run", |ui| ui.label("M7"));
                ui.menu_button("Tools", |ui| ui.label("M7"));
                ui.menu_button("Help", |ui| {
                    if ui.button("About The Editor").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });
            });
        });
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        egui::Window::new("About The Editor")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.heading("The Editor");
                ui.label(format!("Version {}", BUILD.version));
                ui.add_space(6.0);
                ui.label("An IDE for Python and Rust.");
                ui.add_space(6.0);
                ui.label(format!(
                    "Copyright \u{a9} 2026 {}",
                    editor_config::APP_AUTHOR
                ));
                ui.label("Licensed under the MIT licence.");
                ui.add_space(10.0);
                ui.separator();
                egui::Grid::new("about_build")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Build");
                        ui.label(format!("{} ({})", BUILD.commit, BUILD.date));
                        ui.end_row();
                        ui.label("Compiler");
                        ui.label(BUILD.rustc);
                        ui.end_row();
                        ui.label("Config");
                        ui.label(self.paths.config_dir().display().to_string());
                        ui.end_row();
                        ui.label("Logs");
                        ui.label(&self.log_dir);
                        ui.end_row();
                        if self.paths.is_portable() {
                            ui.label("Mode");
                            ui.label("Portable");
                            ui.end_row();
                        }
                    });
            });
        self.show_about = open;
    }
}

impl eframe::App for EditorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Panel order defines nesting: outermost first, CentralPanel last.
        self.menu_bar(ui);

        egui::Panel::top("toolbar").exact_size(34.0).show(ui, |ui| {
            ui.horizontal_centered(|ui| ui.weak("Toolbar \u{2014} M1"));
        });

        egui::Panel::bottom("status_bar")
            .exact_size(22.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.weak("Ready");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(format!("v{}", BUILD.version));
                    });
                });
            });

        egui::Panel::left("explorer")
            .default_size(240.0)
            .size_range(150.0..=600.0)
            .show(ui, |ui| {
                ui.heading("Explorer");
                ui.weak("File tree \u{2014} M1");
            });

        egui::Panel::bottom("dock")
            .resizable(true)
            .default_size(160.0)
            .size_range(80.0..=600.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.weak("OUTPUT");
                    ui.weak("PROBLEMS");
                    ui.weak("SEARCH");
                    ui.weak("TERMINAL");
                });
                ui.separator();
                ui.weak("Output panel \u{2014} M7");
            });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.heading("The Editor");
                ui.label(format!("Version {}", BUILD.version));
                ui.add_space(20.0);
                ui.weak("Editor view \u{2014} M2. Run `cargo spike` for the rendering prototype.");
            });
        });

        let ctx = ui.ctx().clone();
        self.about_window(&ctx);
    }
}
