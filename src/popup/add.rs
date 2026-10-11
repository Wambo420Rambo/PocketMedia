//! Add media inside the popup window: mouse, keyboard and drawing glue between
//! `AddMedia` (state) and `ui::add_view` (geometry and painting).

use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_A, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT,
    VK_SPACE, VK_TAB, VK_UP, VK_V,
};

use super::{Effect, View, hwnd};
use crate::add_media::{AddFocus, AddMedia, RowStatus};
use crate::popup_state::Focus;
use crate::ui::add_view::{self, AddHit, AddLayout, AddUi};
use crate::ui::layout::TextStyle;

const WHEEL_STEP: f32 = 48.0;

impl View {
    pub(super) fn open_add(&mut self) {
        self.add = Some(AddMedia::new());
        self.add_hover = AddHit::None;
        self.add_pressed = AddHit::None;
    }

    /// Back to the gallery with focus on the Add media button (Cancel, Escape, ←).
    fn close_add(&mut self) {
        self.add = None;
        self.state.focus = Focus::AddMedia;
    }

    pub(super) fn add_layout(&mut self) -> Option<AddLayout> {
        let (w, h) = {
            let l = self.ensure_layout();
            (l.width, l.height)
        };
        let a = self.add.as_ref()?;
        Some(AddLayout::compute(a, w, h, &self.gfx))
    }

    pub(super) fn add_paint(&mut self) {
        let Some(l) = self.add_layout() else { return };
        let (caret_x, selection_w) = match self.add.as_ref().map(|a| a.focus) {
            Some(AddFocus::Row(i)) => {
                let row = &self.add.as_ref().expect("checked above").rows[i];
                let text = row.name.text();
                (
                    self.gfx.caret_x(text, TextStyle::Row, row.name.caret_utf16()),
                    self.gfx.measure(text, TextStyle::Row),
                )
            }
            _ => (0.0, 0.0),
        };
        let ui = AddUi { hover: self.add_hover, caret_x, selection_w };
        let dpi = self.dpi as f32;
        let View { add, gfx, .. } = self;
        if let Some(a) = add.as_ref() {
            let _ = gfx.draw(hwnd(), dpi, |p| add_view::paint(p, a, &l, &ui));
        }
    }

    fn clamp_add_scroll(&mut self, show_focus: bool) {
        let Some(l) = self.add_layout() else { return };
        let Some(a) = self.add.as_mut() else { return };
        a.scroll = a.scroll.clamp(0.0, l.max_scroll());
        if show_focus && let Some(r) = l.focus_rect(a) {
            a.scroll = l.scroll_to_show(r, a.scroll);
        }
    }

    pub(super) fn add_hit(&mut self, x: f32, y: f32) -> AddHit {
        match (self.add_layout(), self.add.as_ref()) {
            (Some(l), Some(a)) => l.hit(a, x, y),
            _ => AddHit::None,
        }
    }

    pub(super) fn add_mouse_move(&mut self, x: f32, y: f32) {
        self.add_hover = self.add_hit(x, y);
    }

    pub(super) fn add_mouse_down(&mut self, x: f32, y: f32) -> Effect {
        let hit = self.add_hit(x, y);
        let Some(a) = self.add.as_mut() else { return Effect::None };
        a.focus_visible = false;
        match hit {
            AddHit::RowInput(i) => {
                a.focus = AddFocus::Row(i);
                let input_x = self.add_layout().and_then(|l| l.rows.get(i).and_then(|g| g.input)).map_or(0.0, |r| r.x + 9.0);
                if let Some(a) = self.add.as_mut() {
                    let text = a.rows[i].name.text().to_string();
                    let pos = self.gfx.position_at(&text, TextStyle::Row, x - input_x);
                    a.rows[i].name.set_caret_utf16(pos);
                }
                Effect::None
            }
            AddHit::None | AddHit::Main => Effect::None,
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
                self.add_pressed = other;
                Effect::Capture(true)
            }
        }
    }

    pub(super) fn add_mouse_up(&mut self, x: f32, y: f32) -> Effect {
        let pressed = std::mem::replace(&mut self.add_pressed, AddHit::None);
        if pressed == AddHit::None {
            return Effect::None;
        }
        super::release_capture();
        if self.add_hit(x, y) == pressed { self.add_activate(pressed) } else { Effect::None }
    }

    pub(super) fn add_wheel(&mut self, delta: i16) {
        if let Some(a) = self.add.as_mut() {
            a.scroll -= delta as f32 / 120.0 * WHEEL_STEP;
        }
        self.clamp_add_scroll(false);
    }

    fn add_activate(&mut self, hit: AddHit) -> Effect {
        let reviewing = self.add.as_ref().is_some_and(|a| a.reviewing());
        match hit {
            AddHit::Close => return Effect::Close(true),
            AddHit::Cancel => self.close_add(),
            AddHit::Back if !reviewing => self.close_add(),
            _ => {
                let View { add, state, .. } = self;
                let Some(a) = add.as_mut() else { return Effect::None };
                match hit {
                    AddHit::Back | AddHit::ChooseAgain => a.reset(),
                    AddHit::ChooseFiles | AddHit::ImportExisting => a.choose(&state.items),
                    AddHit::Add => {
                        if let Some(status) = a.import_into(&mut state.items) {
                            *add = None;
                            state.show_imported(status);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.clamp_add_scroll(true);
        Effect::None
    }

    pub(super) fn add_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> Effect {
        if vk == VK_ESCAPE {
            self.close_add();
            return Effect::None;
        }
        let View { add, state, .. } = self;
        let Some(a) = add.as_mut() else { return Effect::None };
        match vk {
            VK_TAB => a.tab(shift),
            _ => match a.focus {
                AddFocus::Row(i) => {
                    let f = &mut a.rows[i].name;
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
                        VK_V if ctrl => return Effect::PasteText,
                        VK_UP | VK_DOWN => {
                            let copy: Vec<usize> =
                                (0..a.rows.len()).filter(|j| a.rows[*j].status == RowStatus::Copy).collect();
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
                        a.edited(i, &state.items);
                    }
                }
                focus if matches!(vk, VK_RETURN | VK_SPACE) => {
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
                    return self.add_activate(hit);
                }
                _ => {}
            },
        }
        self.clamp_add_scroll(true);
        Effect::None
    }

    /// Typed or pasted text for the focused name field.
    pub(super) fn add_insert(&mut self, text: &str) {
        let View { add, state, .. } = self;
        let Some(a) = add.as_mut() else { return };
        if let AddFocus::Row(i) = a.focus {
            a.rows[i].name.insert(text);
            a.edited(i, &state.items);
        }
    }

    /// "screen=add stage=… can_add=… rows=Cat.png:ok|Wave.gif:Taken|…" for the self-test.
    pub(super) fn describe_add(&self) -> Option<String> {
        let a = self.add.as_ref()?;
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
        Some(format!(
            "screen=add stage={} can_add={} label={:?} rows={}",
            if a.reviewing() { "review" } else { "start" },
            a.can_add(),
            a.add_label(),
            rows.join("|")
        ))
    }

    pub(super) fn add_element(&mut self, hit: AddHit) -> Option<crate::ui::layout::Rect> {
        let l = self.add_layout()?;
        let a = self.add.as_ref()?;
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
            AddHit::Main => crate::ui::layout::Rect::new(l.main.x + l.main.w - 40.0, l.main.y + 4.0, 10.0, 8.0),
            _ => return None,
        })
    }
}
