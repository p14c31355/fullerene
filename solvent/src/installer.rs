//! Graphical Fullerene installer wizard.
//!
//! The wizard owns presentation and input state in Solvent.  The kernel owns
//! disk discovery and the destructive install callback; the callback is always
//! invoked outside the runtime lock by `service_install_request`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::{FB_DIMS, RUNTIME_CONTEXT, RuntimeState};
use lattice::painter::Painter;
use lattice::window::WindowId;

const WINDOW_WIDTH: u32 = 650;
const WINDOW_HEIGHT: u32 = 450;
const BUTTON_Y: i32 = 370;
const BUTTON_WIDTH: u32 = 130;
const BUTTON_HEIGHT: u32 = 38;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallerPage {
    Welcome,
    SelectDisk,
    Confirm,
    Installing,
    Complete,
    Failed,
}

pub(crate) struct InstallerState {
    pub(crate) window_id: WindowId,
    pub(crate) devices: Vec<crate::InstallerDevice>,
    pub(crate) selected: Option<usize>,
    pub(crate) page: InstallerPage,
    pub(crate) pending_device: Option<String>,
    pub(crate) install_deferred: bool,
    pub(crate) message: String,
}

fn discover_devices() -> Vec<crate::InstallerDevice> {
    RUNTIME_CONTEXT
        .callback_snapshot()
        .installer_device_list
        .map(|list| list())
        .unwrap_or_default()
}

pub(crate) fn open(rt: &mut RuntimeState) {
    if let Some(state) = rt.installer.as_ref()
        && rt
            .desktop
            .wm
            .windows()
            .iter()
            .any(|w| w.id == state.window_id)
    {
        rt.desktop.wm.raise_to_top(state.window_id);
        rt.installer_dirty = true;
        rt.frame_due = true;
        return;
    }

    let devices = discover_devices();
    let (fb_width, fb_height, _) = *FB_DIMS.lock();
    let work_top = rt.desktop.top_panel_offset();
    let work_height = rt
        .desktop
        .work_area(fb_width.max(800), fb_height.max(600))
        .1;
    let x = fb_width.saturating_sub(WINDOW_WIDTH) / 2;
    let y = work_top + work_height.saturating_sub(WINDOW_HEIGHT) / 2;
    let id = rt.desktop.wm.create_titled_window(
        x as i32,
        y as i32,
        WINDOW_WIDTH,
        WINDOW_HEIGHT,
        0xF7F9FC,
        "Install Fullerene",
    );
    rt.desktop.wm.raise_to_top(id);
    rt.installer = Some(InstallerState {
        window_id: id,
        devices,
        selected: None,
        page: InstallerPage::Welcome,
        pending_device: None,
        install_deferred: false,
        message: String::new(),
    });
    rt.installer_dirty = true;
    rt.desktop.force_full_redraw();
    rt.frame_due = true;
}

pub(crate) fn handle_mouse(rt: &mut RuntimeState, x: i32, y: i32) -> bool {
    let Some(state) = rt.installer.as_mut() else {
        return false;
    };
    let id = state.window_id;
    let Some(window) = rt.desktop.wm.windows().iter().find(|w| w.id == id) else {
        rt.installer = None;
        rt.installer_dirty = false;
        return false;
    };
    if !window.contains(x, y) {
        return false;
    }

    let rel_x = x - window.x;
    let rel_y = y - window.y - lattice::style::title_bar_height() as i32;

    // A visible Cancel button is present on every page except the final
    // result screens. It only closes the wizard; it never touches the disk.
    if rel_x >= 36
        && rel_x < 36 + BUTTON_WIDTH as i32
        && rel_y >= BUTTON_Y
        && rel_y < BUTTON_Y + BUTTON_HEIGHT as i32
        && !matches!(state.page, InstallerPage::Installing)
    {
        rt.desktop.wm.close_window(id);
        rt.installer = None;
        rt.installer_dirty = false;
        rt.desktop.force_full_redraw();
        rt.frame_due = true;
        return true;
    }

    match state.page {
        InstallerPage::Welcome => {
            if button_hit(rel_x, rel_y, 484, BUTTON_Y) {
                // AHCI may have been initialized from the shell while the
                // welcome page was open. Refresh here so the target list
                // reflects the current /dev block-device registry.
                state.devices = discover_devices();
                state.selected = None;
                state.page = InstallerPage::SelectDisk;
            }
        }
        InstallerPage::SelectDisk => {
            let list_y = 132i32;
            let row_height = 48i32;
            if rel_y >= list_y
                && rel_y < BUTTON_Y
                && rel_y < list_y + state.devices.len() as i32 * row_height
            {
                let row = ((rel_y - list_y) / row_height) as usize;
                if row < state.devices.len() {
                    let device = &state.devices[row];
                    if device.available && device.sector_size == 512 {
                        state.selected = Some(row);
                    }
                }
            }
            if button_hit(rel_x, rel_y, 484, BUTTON_Y) {
                if state.selected.is_some() {
                    state.page = InstallerPage::Confirm;
                }
            }
        }
        InstallerPage::Confirm => {
            if button_hit(rel_x, rel_y, 484, BUTTON_Y) {
                if let Some(index) = state.selected {
                    if let Some(device) = state.devices.get(index) {
                        state.pending_device = Some(device.name.clone());
                        state.install_deferred = true;
                        state.message = format!("Installing to /dev/{}…", device.name);
                        state.page = InstallerPage::Installing;
                    }
                }
            }
        }
        InstallerPage::Installing => {}
        InstallerPage::Complete | InstallerPage::Failed => {
            if button_hit(rel_x, rel_y, 484, BUTTON_Y) {
                rt.desktop.wm.close_window(id);
                rt.installer = None;
                rt.installer_dirty = false;
                rt.desktop.force_full_redraw();
            }
        }
    }
    rt.installer_dirty = true;
    rt.frame_due = true;
    true
}

fn button_hit(x: i32, y: i32, button_x: i32, button_y: i32) -> bool {
    x >= button_x
        && x < button_x + BUTTON_WIDTH as i32
        && y >= button_y
        && y < button_y + BUTTON_HEIGHT as i32
}

pub(crate) fn service_install_request() {
    let device = RUNTIME_CONTEXT.runtime().as_mut().and_then(|rt| {
        rt.installer.as_mut().and_then(|state| {
            if state.page == InstallerPage::Installing {
                if state.install_deferred {
                    state.install_deferred = false;
                    return None;
                }
                state.pending_device.clone()
            } else {
                None
            }
        })
    });
    let Some(device) = device else {
        return;
    };

    let callback = RUNTIME_CONTEXT.callback_snapshot().installer_run;
    let result = callback
        .map(|install| install(&device))
        .unwrap_or_else(|| Err(String::from("installer service unavailable")));

    if let Some(rt) = RUNTIME_CONTEXT.runtime().as_mut()
        && let Some(state) = rt.installer.as_mut()
    {
        match result {
            Ok(progress) if progress.complete => {
                state.page = InstallerPage::Complete;
                state.pending_device = None;
                state.message = format!(
                    "Installation complete. {} payload bytes were written to /dev/{}.",
                    progress.written_bytes, device
                );
            }
            Ok(progress) => {
                state.message = format!(
                    "Installing to /dev/{}… {} / {} bytes",
                    device, progress.written_bytes, progress.total_bytes
                );
            }
            Err(error) => {
                state.page = InstallerPage::Failed;
                state.message = format!("Installation failed: {}", error);
            }
        }
        rt.installer_dirty = true;
        rt.frame_due = true;
    }
}

pub(crate) fn render(rt: &mut RuntimeState) {
    let (id, page, selected, devices, message) = match rt.installer.as_ref() {
        Some(state) => (
            state.window_id,
            state.page,
            state.selected,
            state.devices.clone(),
            state.message.clone(),
        ),
        None => return,
    };
    let Some(window) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) else {
        rt.installer = None;
        rt.installer_dirty = false;
        return;
    };

    let width = window.surface.width();
    let height = window.surface.height();
    let pixels = window.surface.pixels_mut();
    let mut painter = Painter::new(pixels, width, height);
    painter.fill_rect(0, 0, width, height, 0xF7F9FC);

    painter.draw_text(32, 28, "Install Fullerene", 0x17324D, 26.0);
    painter.draw_text(32, 62, page_subtitle(page), 0x53677A, 15.0);

    match page {
        InstallerPage::Welcome => {
            painter.draw_text(32, 120, "Install Fullerene on a SATA SSD", 0x1F3448, 19.0);
            draw_lines(
                &mut painter,
                32,
                164,
                &[
                    "This wizard will copy the running system to an EFI-bootable disk.",
                    "The selected disk will be erased and reformatted.",
                    "Only registered AHCI disks with 512-byte sectors are shown.",
                ],
                0x354B5E,
            );
        }
        InstallerPage::SelectDisk => {
            painter.draw_text(32, 104, "Choose the disk to install to", 0x1F3448, 18.0);
            if devices.is_empty() {
                painter.draw_text(
                    32,
                    156,
                    "No AHCI block devices are available.",
                    0xA13B3B,
                    16.0,
                );
                painter.draw_text(
                    32,
                    188,
                    "Check the SATA connection and run the wizard again.",
                    0x53677A,
                    14.0,
                );
            } else {
                for (index, device) in devices.iter().enumerate() {
                    let y = 132 + index as i32 * 48;
                    let usable = device.available && device.sector_size == 512;
                    let bg = if selected == Some(index) {
                        0xCFE8FF
                    } else if usable {
                        0xE9F0F6
                    } else {
                        0xE2E5E8
                    };
                    painter.rounded_rect(32, y, 586, 38, 6, bg);
                    let color = if usable { 0x1F3448 } else { 0x7D8790 };
                    painter.draw_text(48, y + 8, &format!("/dev/{}", device.name), color, 15.0);
                    painter.draw_text(
                        250,
                        y + 8,
                        &format!(
                            "{}  •  {}",
                            size_string(device.total_sectors),
                            device_status(device)
                        ),
                        color,
                        14.0,
                    );
                }
            }
        }
        InstallerPage::Confirm => {
            painter.draw_text(32, 110, "Ready to install", 0x1F3448, 20.0);
            let target = selected
                .and_then(|index| devices.get(index))
                .map(|device| {
                    format!(
                        "/dev/{} ({})",
                        device.name,
                        size_string(device.total_sectors)
                    )
                })
                .unwrap_or_else(|| String::from("(no target selected)"));
            painter.draw_text(32, 158, &format!("Target: {}", target), 0x1F3448, 17.0);
            painter.rounded_rect(32, 200, 586, 94, 8, 0xFFE4E0);
            draw_lines(
                &mut painter,
                50,
                222,
                &[
                    "Warning: all data on this disk will be destroyed.",
                    "Disconnect any disk you do not want to erase.",
                    "Press Install only when the target above is correct.",
                ],
                0x8E3028,
            );
        }
        InstallerPage::Installing => {
            painter.draw_text(32, 130, "Installing Fullerene…", 0x1F3448, 22.0);
            painter.rounded_rect(32, 190, 586, 24, 12, 0xDCE5EC);
            painter.rounded_rect(32, 190, 180, 24, 12, 0x3A8DCC);
            painter.draw_text(32, 246, &message, 0x53677A, 15.0);
            painter.draw_text(32, 278, "Do not power off the machine.", 0x8E3028, 15.0);
        }
        InstallerPage::Complete | InstallerPage::Failed => {
            let success = page == InstallerPage::Complete;
            painter.draw_text(
                32,
                130,
                if success {
                    "Installation complete"
                } else {
                    "Installation failed"
                },
                if success { 0x237A45 } else { 0xA13B3B },
                22.0,
            );
            draw_wrapped(&mut painter, 32, 188, &message, 72, 0x354B5E);
            if success {
                painter.draw_text(
                    32,
                    292,
                    "Reboot and remove the installation media.",
                    0x53677A,
                    15.0,
                );
            }
        }
    }

    if !matches!(page, InstallerPage::Installing) {
        button(&mut painter, 36, BUTTON_Y, "Cancel", false);
    }
    let (label, enabled) = match page {
        InstallerPage::Welcome => ("Continue", true),
        InstallerPage::SelectDisk => ("Continue", selected.is_some()),
        InstallerPage::Confirm => ("Install", selected.is_some()),
        InstallerPage::Installing => ("Installing…", false),
        InstallerPage::Complete | InstallerPage::Failed => ("Close", true),
    };
    button(&mut painter, 484, BUTTON_Y, label, enabled);
    // The wizard owns the window surface, so repainting it is not enough to
    // make the compositor copy the new page into the framebuffer. Mark the
    // window dirty after every page/progress update.
    rt.desktop.invalidate_window(id);
    rt.installer_dirty = false;
}

fn page_subtitle(page: InstallerPage) -> &'static str {
    match page {
        InstallerPage::Welcome => "Welcome",
        InstallerPage::SelectDisk => "Step 1 of 2 · Select a target",
        InstallerPage::Confirm => "Step 2 of 2 · Confirm installation",
        InstallerPage::Installing => "Writing system files",
        InstallerPage::Complete => "Finished",
        InstallerPage::Failed => "Please review the error",
    }
}

fn button(painter: &mut Painter<'_>, x: i32, y: i32, label: &str, enabled: bool) {
    painter.rounded_rect(
        x,
        y,
        BUTTON_WIDTH,
        BUTTON_HEIGHT,
        8,
        if enabled { 0x2B76B9 } else { 0xB8C3CC },
    );
    painter.draw_text(x + 18, y + 10, label, 0xFFFFFF, 15.0);
}

fn draw_lines(painter: &mut Painter<'_>, x: i32, y: i32, lines: &[&str], color: u32) {
    for (index, line) in lines.iter().enumerate() {
        painter.draw_text(x, y + index as i32 * 30, line, color, 15.0);
    }
}

fn draw_wrapped(painter: &mut Painter<'_>, x: i32, y: i32, text: &str, width: usize, color: u32) {
    let mut line = String::new();
    let mut row = 0;
    for word in text.split_whitespace() {
        if line.len() + word.len() + 1 > width && !line.is_empty() {
            painter.draw_text(x, y + row * 28, &line, color, 15.0);
            row += 1;
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        painter.draw_text(x, y + row * 28, &line, color, 15.0);
    }
}

fn size_string(sectors: u64) -> String {
    let kib = sectors / 2;
    if kib >= 1024 * 1024 {
        format!(
            "{}.{:01} GiB",
            kib / (1024 * 1024),
            (kib % (1024 * 1024)) * 10 / (1024 * 1024)
        )
    } else {
        format!("{} MiB", kib / 1024)
    }
}

fn device_status(device: &crate::InstallerDevice) -> &'static str {
    if !device.available {
        "busy"
    } else if device.sector_size != 512 {
        "unsupported sector size"
    } else {
        "available"
    }
}
