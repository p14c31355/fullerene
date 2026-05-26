use crate::cursor::Cursor;
use crate::window::Window;

/// An immutable snapshot of the desktop state for the compositor.
///
/// `Scene` is pure data — it carries references to whatever the compositor
/// needs to draw, without owning or mutating anything.
///
/// ```ignore
/// let scene = desktop.scene();
/// compositor.render(&scene, &mut target);
/// ```
#[derive(Clone)]
pub struct Scene<'a> {
    pub windows: &'a [Window],
    pub cursor: Option<&'a Cursor>,
    pub bg_color: u32,
}

impl<'a> Scene<'a> {
    pub fn new(windows: &'a [Window], cursor: Option<&'a Cursor>, bg_color: u32) -> Self {
        Self {
            windows,
            cursor,
            bg_color,
        }
    }
}
