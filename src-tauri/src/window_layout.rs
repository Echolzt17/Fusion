//! Keep the main window usable on small work areas and mixed-DPI monitors.
use std::sync::Mutex;
use tauri::{Manager, PhysicalPosition, PhysicalSize};

#[derive(Clone, Copy, PartialEq)]
struct WorkArea {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale: f64,
}
#[derive(Default)]
pub struct Layout(Mutex<Option<WorkArea>>);

fn sizes(mut work: WorkArea, current: (u32, u32), frame: (u32, u32)) -> ((u32, u32), (u32, u32)) {
    work.width = work.width.saturating_sub(frame.0).max(1);
    work.height = work.height.saturating_sub(frame.1).max(1);
    let minimum = (
        ((840. * work.scale).round() as u32).min(work.width),
        ((620. * work.scale).round() as u32).min(work.height),
    );
    let size = (
        current.0.clamp(minimum.0, work.width),
        current.1.clamp(minimum.1, work.height),
    );
    (minimum, size)
}

pub fn fit(w: &tauri::WebviewWindow) -> tauri::Result<()> {
    let Some(monitor) = w.current_monitor()? else {
        return Ok(());
    };
    let area = monitor.work_area();
    let work = WorkArea {
        x: area.position.x,
        y: area.position.y,
        width: area.size.width,
        height: area.size.height,
        scale: monitor.scale_factor(),
    };
    if work.width == 0 || work.height == 0 {
        return Ok(());
    }
    let Some(state) = w.try_state::<Layout>() else {
        return Ok(());
    };
    {
        let mut previous = state.0.lock().unwrap_or_else(|e| e.into_inner());
        if *previous == Some(work) {
            return Ok(());
        }
        // Set before changing native geometry to prevent recursive move/resize events.
        *previous = Some(work);
    }
    let result = (|| {
        let current = w.inner_size()?;
        let outer = w.outer_size()?;
        let frame = (
            outer.width.saturating_sub(current.width),
            outer.height.saturating_sub(current.height),
        );
        let (minimum, size) = sizes(work, (current.width, current.height), frame);
        w.set_min_size(Some(PhysicalSize::new(minimum.0, minimum.1)))?;
        w.set_max_size(Some(PhysicalSize::new(
            work.width.saturating_sub(frame.0).max(1),
            work.height.saturating_sub(frame.1).max(1),
        )))?;
        if w.is_maximized()? || w.is_fullscreen()? {
            return Ok(());
        }
        if size != (current.width, current.height) {
            w.set_size(PhysicalSize::new(size.0, size.1))?;
        }
        let outer = w.outer_size()?;
        let position = w.outer_position()?;
        let x = position.x.clamp(
            work.x,
            work.x + work.width.saturating_sub(outer.width) as i32,
        );
        let y = position.y.clamp(
            work.y,
            work.y + work.height.saturating_sub(outer.height) as i32,
        );
        if (position.x, position.y) != (x, y) {
            w.set_position(PhysicalPosition::new(x, y))?;
        }
        Ok(())
    })();
    if result.is_err() {
        *state.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_laptop_work_area_always_fits_at_all_supported_scales() {
        for scale in [1., 1.25, 1.5, 1.75, 2.] {
            for (width, height) in [(1920, 1040), (1366, 728), (800, 560)] {
                let work = WorkArea {
                    x: -1920,
                    y: -200,
                    width,
                    height,
                    scale,
                };
                let (min, size) = sizes(
                    work,
                    ((1080. * scale) as u32, (760. * scale) as u32),
                    (16, 16),
                );
                assert!(min.0 <= width && min.1 <= height);
                assert!(size.0 + 16 <= width && size.1 + 16 <= height);
                assert!(size.0 >= min.0 && size.1 >= min.1);
            }
        }
    }
}
