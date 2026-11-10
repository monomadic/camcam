//! Native menus are created and retained on the application's main thread.
use eframe::egui;
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct NativeMenu {
    _menu: Menu,
    scan: Arc<AtomicBool>,
}

impl NativeMenu {
    pub fn new(context: &egui::Context) -> anyhow::Result<Self> {
        let menu = Menu::new();
        let app = Submenu::with_items(
            "CamCam",
            true,
            &[
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(None),
            ],
        )?;
        let edit = Submenu::with_items(
            "Edit",
            true,
            &[
                &PredefinedMenuItem::undo(None),
                &PredefinedMenuItem::redo(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::cut(None),
                &PredefinedMenuItem::copy(None),
                &PredefinedMenuItem::paste(None),
                &PredefinedMenuItem::select_all(None),
            ],
        )?;
        let scan_item = MenuItem::new("Scan…", true, None);
        let tools = Submenu::with_items("Tools", true, &[&scan_item])?;
        menu.append_items(&[&app, &edit, &tools])?;
        let scan = Arc::new(AtomicBool::new(false));
        let requested = scan.clone();
        let context = context.clone();
        let id = scan_item.id().clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == id {
                requested.store(true, Ordering::Release);
                context.request_repaint();
            }
        }));
        menu.init_for_nsapp();
        Ok(Self { _menu: menu, scan })
    }

    pub fn take_scan(&self) -> bool {
        self.scan.swap(false, Ordering::AcqRel)
    }
}
