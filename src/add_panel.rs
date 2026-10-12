//! The Add media panel as a reusable piece: any window (the popup or the
//! Manager) can host it. Mouse, keyboard and drawing glue between `AddMedia`
//! (state), `ui::add_view` (geometry and painting) and the shared library.
//! The host window turns each `AddOutcome` into window actions.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_A, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT,
    VK_SPACE, VK_TAB, VK_UP, VK_V,
};

use crate::add_media::{AddFocus, AddMedia, RowStatus};
use crate::library;
use crate::ui::add_view::{self, AddHit, AddLayout, AddUi};
use crate::ui::d2d::Gfx;
use crate::ui::layout::{Rect, TextStyle};

const WHEEL_STEP: f32 = 48.0;

/// What the host window should do after an input event.
pub enum AddOutcome {
    None,
    /// The window's × was clicked.
    CloseWindow,
    /// Cancel, ← on the start page, or Escape: back to the window's own content.
    Leave,
    /// Files were added; the status line describes the result.
    Imported(String),
    /// Start or stop mouse capture (a button is being pressed).
    Capture(bool),
    /// Ctrl+V in a name field: the host reads the clipboard and calls `insert`.
    PasteText,
}

/// The window size the panel is laid out in, in DIPs.
#[derive(Clone, Copy)]
pub struct Area {
    pub w: f32,
    pub h: f32,
}

pub struct AddPanel {
    pub add: AddMedia,
    pub hover: AddHit,
    pressed: AddHit,
}

impl Default for AddPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl AddPanel {
    pub fn new() -> Self {
        Self { add: AddMedia::new(), hover: AddHit::None, pressed: AddHit::None }
    }

    pub fn layout(&self, gfx: &Gfx, area: Area) -> AddLayout {
        AddLayout::compute(&self.add, area.w, area.h, gfx)
    }

    pub fn paint(&self, gfx: &mut Gfx, hwnd: HWND, dpi: f32, area: Area) {
        let l = self.layout(gfx, area);
        let (caret_x, selection_w) = match self.add.focus {
            AddFocus::Row(i) => {
                let name = &self.add.rows[i].name;
                (gfx.caret_x(name.text(), TextStyle::Row, name.caret_utf16()), gfx.measure(name.text(), TextStyle::Row))
            }
            _ => (0.0, 0.0),
        };
        let ui = AddUi { hover: self.hover, caret_x, selection_w };
        let _ = gfx.draw(hwnd, dpi, |p| add_view::paint(p, &self.add, &l, &ui));
    }

    /// The mouse cursor should be an I-beam (over a name field).
    pub fn text_hover(&self) -> bool {
        matches!(self.hover, AddHit::RowInput(_))
    }

    pub fn clear_hover(&mut self) {
        self.hover = AddHit::None;
        self.pressed = AddHit::None;
    }

    fn hit(&self, gfx: &Gfx, area: Area, x: f32, y: f32) -> AddHit {
        self.layout(gfx, area).hit(&self.add, x, y)
    }

    fn clamp_scroll(&mut self, gfx: &Gfx, area: Area, show_focus: bool) {
        let l = self.layout(gfx, area);
        let a = &mut self.add;
        a.scroll = a.scroll.clamp(0.0, l.max_scroll());
        if show_focus && let Some(r) = l.focus_rect(a) {
            a.scroll = l.scroll_to_show(r, a.scroll);
        }
    }

    pub fn mouse_move(&mut self, gfx: &Gfx, area: Area, x: f32, y: f32) {
        self.hover = self.hit(gfx, area, x, y);
    }

    pub fn mouse_down(&mut self, gfx: &Gfx, area: Area, x: f32, y: f32) -> AddOutcome {
        let l = self.layout(gfx, area);
        let hit = l.hit(&self.add, x, y);
        let a = &mut self.add;
        a.focus_visible = false;
        match hit {
            AddHit::RowInput(i) => {
                a.focus = AddFocus::Row(i);
                let input_x = l.rows.get(i).and_then(|g| g.input).map_or(0.0, |r| r.x + 9.0);
                let name = &mut a.rows[i].name;
                let pos = gfx.position_at(name.text(), TextStyle::Row, x - input_x);
                name.set_caret_utf16(pos);
                AddOutcome::None
            }
            AddHit::None | AddHit::Main => AddOutcome::None,
            other => {
                a.focus = match other {
                    AddHit::Close => AddFocus::Close,
                    AddHit::Back => AddFocus::Back,
                    AddHit::ChooseFiles => AddFocus::ChooseFiles,
                    AddHit::ImportExisting => AddFocus::ImportExisting,
                    AddHit::ChooseAgain => AddFocus::ChooseAgain,
                    AddHit::Cancel => AddFocus::Cancel,
                    _ => AddFocus::Add,
                };
                self.pressed = other;
                AddOutcome::Capture(true)
            }
        }
    }

    pub fn mouse_up(&mut self, gfx: &Gfx, area: Area, x: f32, y: f32) -> AddOutcome {
        let pressed = std::mem::replace(&mut self.pressed, AddHit::None);
        if pressed == AddHit::None {
            return AddOutcome::None;
        }
        if self.hit(gfx, area, x, y) == pressed {
            match self.activate(gfx, area, pressed) {
                AddOutcome::None => AddOutcome::Capture(false),
                other => other,
            }
        } else {
            AddOutcome::Capture(false)
        }
    }

    pub fn wheel(&mut self, gfx: &Gfx, area: Area, delta: i16) {
        self.add.scroll -= delta as f32 / 120.0 * WHEEL_STEP;
        self.clamp_scroll(gfx, area, false);
    }

    fn activate(&mut self, gfx: &Gfx, area: Area, hit: AddHit) -> AddOutcome {
        let a = &mut self.add;
        match hit {
            AddHit::Close => return AddOutcome::CloseWindow,
            AddHit::Cancel => return AddOutcome::Leave,
            AddHit::Back if !a.reviewing() => return AddOutcome::Leave,
            AddHit::Back | AddHit::ChooseAgain => a.reset(),
            AddHit::ChooseFiles | AddHit::ImportExisting => library::with(|lib| a.choose(&lib.items)),
            AddHit::Add => {
                if let Some(status) = library::with_mut(|lib| a.import_into(&mut lib.items)) {
                    return AddOutcome::Imported(status);
                }
            }
            _ => {}
        }
        self.clamp_scroll(gfx, area, true);
        AddOutcome::None
    }

    pub fn key(&mut self, gfx: &Gfx, area: Area, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> AddOutcome {
        if vk == VK_ESCAPE {
            return AddOutcome::Leave;
        }
        let a = &mut self.add;
        match (vk, a.focus) {
            (VK_TAB, _) => a.tab(shift),
            (_, AddFocus::Row(i)) => {
                let f = &mut a.rows[i].name;
                // Caret moves leave the name as it is; edits re-validate the batch.
                let edited = match vk {
                    VK_LEFT => {
                        f.left();
                        false
                    }
                    VK_RIGHT => {
                        f.right();
                        false
                    }
                    VK_HOME => {
                        f.home();
                        false
                    }
                    VK_END => {
                        f.end();
                        false
                    }
                    VK_A if ctrl => {
                        f.select_all();
                        false
                    }
                    VK_BACK if ctrl => {
                        f.delete_word_back();
                        true
                    }
                    VK_BACK => {
                        f.backspace();
                        true
                    }
                    VK_DELETE => {
                        f.delete();
                        true
                    }
                    VK_V if ctrl => return AddOutcome::PasteText,
                    VK_UP | VK_DOWN => {
                        let copy: Vec<usize> = (0..a.rows.len()).filter(|j| a.rows[*j].status == RowStatus::Copy).collect();
                        let pos = copy.iter().position(|j| *j == i).unwrap_or(0);
                        let next = if vk == VK_UP { pos.checked_sub(1) } else { Some(pos + 1) };
                        if let Some(&j) = next.and_then(|n| copy.get(n)) {
                            a.focus = AddFocus::Row(j);
                            a.focus_visible = true;
                        }
                        false
                    }
                    _ => false,
                };
                if edited {
                    library::with(|lib| a.edited(i, &lib.items));
                }
            }
            (VK_RETURN | VK_SPACE, focus) => {
                let hit = match focus {
                    AddFocus::Close => AddHit::Close,
                    AddFocus::Back => AddHit::Back,
                    AddFocus::ChooseFiles => AddHit::ChooseFiles,
                    AddFocus::ImportExisting => AddHit::ImportExisting,
                    AddFocus::ChooseAgain => AddHit::ChooseAgain,
                    AddFocus::Cancel => AddHit::Cancel,
                    AddFocus::Add => AddHit::Add,
                    AddFocus::Row(_) => AddHit::None,
                };
                return self.activate(gfx, area, hit);
            }
            _ => {}
        }
        self.clamp_scroll(gfx, area, true);
        AddOutcome::None
    }

    /// Typed or pasted text for the focused name field.
    pub fn insert(&mut self, text: &str) {
        let a = &mut self.add;
        if let AddFocus::Row(i) = a.focus {
            a.rows[i].name.insert(text);
            library::with(|lib| a.edited(i, &lib.items));
        }
    }

    /// "screen=add stage=… can_add=… rows=Cat.png:ok|Wave.gif:Taken|…" for the self-test.
    pub fn describe(&self) -> String {
        let a = &self.add;
        let rows: Vec<String> = a
            .rows
            .iter()
            .map(|r| {
                let state = match (r.status, r.error) {
                    (RowStatus::Skip, _) => "skip".to_string(),
                    (_, Some(e)) => format!("{e:?}"),
                    _ => "ok".into(),
                };
                format!("{}{}:{state}", r.name.text(), r.ext())
            })
            .collect();
        format!(
            "screen=add stage={} can_add={} label={:?} rows={}",
            if a.reviewing() { "review" } else { "start" },
            a.can_add(),
            a.add_label(),
            rows.join("|")
        )
    }

    /// Where an element is, for the self-test to click.
    pub fn element(&self, gfx: &Gfx, area: Area, hit: AddHit) -> Option<Rect> {
        let l = self.layout(gfx, area);
        let a = &self.add;
        Some(match hit {
            AddHit::Close => l.close,
            AddHit::Back => l.back,
            AddHit::ChooseFiles if !a.reviewing() => l.choose_files,
            AddHit::ImportExisting if !a.reviewing() => l.import_existing,
            AddHit::RowInput(i) => l.rows.get(i)?.input?,
            AddHit::ChooseAgain if a.reviewing() => l.choose_again,
            AddHit::Cancel if a.reviewing() => l.cancel,
            AddHit::Add if a.reviewing() => l.add,
            // Somewhere in the scrolling area that is not a control: the summary line.
            AddHit::Main => Rect::new(l.main.x + l.main.w - 40.0, l.main.y + 4.0, 10.0, 8.0),
            _ => return None,
        })
    }
}
