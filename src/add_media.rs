//! Add media flow state (Focused panel): the start page, the review list with an
//! editable name per file, and progress while copying. No disk access here: the
//! scan, plan and copies run in `import.rs` on a worker thread (`jobs.rs`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::import::{self, Copied, CopyRequest, Plan, Status};
use crate::names::{self, NameError};
use crate::sample::MediaKind;
use crate::text_field::TextField;

pub const AUTO_RENAMED: &str = "A file with this name already exists, so a number was added. You can change it.";
pub const UNSUPPORTED: &str = "Not a supported format (JPG, JPEG, PNG, GIF or MP4).";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowStatus {
    Copy,
    /// Not imported: identical content already there, or not a supported format.
    Skip,
}

#[derive(Debug)]
pub struct ImportRow {
    /// The source file name with extension.
    pub source: String,
    pub path: PathBuf,
    pub size: u64,
    pub kind: Option<MediaKind>,
    pub status: RowStatus,
    /// For a row skipped as the same file as an earlier row of this batch: that row.
    pub dup_of: Option<usize>,
    /// Why a Skip row is skipped.
    pub skip_reason: Option<String>,
    /// The file or folder could not be read.
    pub unreadable: bool,
    /// Editable name without the extension (copy rows only).
    pub name: TextField,
    /// A number was added because the name was taken; cleared once the user edits.
    pub auto_renamed: bool,
    pub error: Option<NameError>,
    /// After copying: the final name, or why it failed.
    pub outcome: Option<Result<String, String>>,
}

impl ImportRow {
    pub fn ext(&self) -> &str {
        import::ext_of(&self.source)
    }

    pub fn destination(&self) -> &'static str {
        self.kind.map_or("", MediaKind::folder)
    }

    pub fn unsupported(&self) -> bool {
        self.kind.is_none() && !self.unreadable
    }

    /// The line under the name, if any.
    pub fn message(&self) -> Option<String> {
        if let Some(Err(e)) = &self.outcome {
            return Some(format!("Not added: {e}"));
        }
        match (self.status, self.error) {
            (RowStatus::Skip, _) => self.skip_reason.clone(),
            (_, Some(e)) => Some(e.message().into()),
            _ if self.auto_renamed => Some(AUTO_RENAMED.into()),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Start,
    /// Download from Twitter: the post links.
    Links,
    /// Downloading the links' pictures, videos and GIFs (then Scanning, as for chosen files).
    Downloading { progress: Option<crate::download::Progress>, stopping: bool },
    /// Listing and checking the chosen files.
    Scanning,
    Review,
    Copying { done: usize, total: usize, stopping: bool },
}

/// When to save the library file during an import: after every
/// `EVERY` copies or `AFTER` time, and once at the end, instead of after each
/// file. A crash in between loses no file: the startup comparison adds finished
/// copies that have no record yet.
#[derive(Debug, Default)]
pub struct SaveBatch {
    unsaved: usize,
    since: Option<Instant>,
}

impl SaveBatch {
    pub const EVERY: usize = 100;
    pub const AFTER: Duration = Duration::from_secs(3);

    /// A copy was recorded at `now`. True when the library file should be saved now.
    pub fn copied(&mut self, now: Instant) -> bool {
        self.unsaved += 1;
        let since = *self.since.get_or_insert(now);
        if self.unsaved >= Self::EVERY || now.duration_since(since) >= Self::AFTER {
            *self = Self::default();
            return true;
        }
        false
    }

    /// The import ended. True when a recorded copy is not saved yet.
    pub fn finished(&mut self) -> bool {
        let any = self.unsaved > 0;
        *self = Self::default();
        any
    }
}

/// Keyboard focus inside the Add media panel, in Tab order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddFocus {
    Close,
    Back,
    ChooseFiles,
    ImportExisting,
    DownloadTwitter,
    /// A post link's text box, and the × that removes it.
    Link(usize),
    RemoveLink(usize),
    AddLink,
    PasteLinks,
    Row(usize),
    /// The × that leaves row `i` out of the import.
    Remove(usize),
    ChooseAgain,
    Cancel,
    Add,
}

pub struct AddMedia {
    pub rows: Vec<ImportRow>,
    pub stage: Stage,
    pub focus: AddFocus,
    pub focus_visible: bool,
    pub scroll: f32,
    /// Shown on the start page when choosing failed (e.g. an unreadable folder).
    pub error: Option<String>,
    /// Shown in the footer on every page, in place of the safety line: something
    /// waits for this panel to close (the uninstaller's Back up library).
    pub notice: Option<String>,
    /// Download from Twitter: the post links, and why a link was not downloaded.
    pub links: Vec<TextField>,
    pub link_errors: Vec<Option<String>>,
    /// The files under review were downloaded: ← and Choose again go back to
    /// the links, and nothing is said about originals.
    pub from_download: bool,
    /// Links that gave nothing, counted in the review's summary.
    pub link_failures: usize,
}

pub const NOT_A_LINK: &str = "Not a link to a post on Twitter.";

/// The uninstaller asked for Back up library while Add media was open.
pub const BACKUP_WAITS: &str = "Back up library is waiting: finish or cancel Add media first, then choose Back up library in Settings.";

impl Default for AddMedia {
    fn default() -> Self {
        Self::new()
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

impl AddMedia {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            stage: Stage::Start,
            focus: AddFocus::ChooseFiles,
            focus_visible: false,
            scroll: 0.0,
            error: None,
            notice: None,
            links: Vec::new(),
            link_errors: Vec::new(),
            from_download: false,
            link_failures: 0,
        }
    }

    /// The pages about files: checking, review and copying.
    pub fn reviewing(&self) -> bool {
        matches!(self.stage, Stage::Scanning | Stage::Review | Stage::Copying { .. })
    }

    /// The Download from Twitter page (while editing the links or downloading).
    pub fn links_page(&self) -> bool {
        matches!(self.stage, Stage::Links | Stage::Downloading { .. })
    }

    /// Work is running on the worker thread.
    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Scanning | Stage::Downloading { .. } | Stage::Copying { .. })
    }

    /// Names can be edited only while reviewing.
    pub fn editable(&self) -> bool {
        self.stage == Stage::Review
    }

    pub fn begin_scan(&mut self) {
        self.rows.clear();
        self.error = None;
        self.scroll = 0.0;
        self.stage = Stage::Scanning;
        self.focus = AddFocus::Cancel;
    }

    pub fn scan_failed(&mut self, message: String) {
        self.reset();
        self.error = Some(message);
    }

    /// Shows the plan for review. `library` is every library file name.
    pub fn load_plan(&mut self, plan: &Plan, library: &[&str]) {
        self.rows = plan
            .rows
            .iter()
            .map(|p| {
                let (status, skip_reason) = match &p.status {
                    Status::Copy => (RowStatus::Copy, None),
                    Status::InLibrary(name) => (RowStatus::Skip, Some(format!("Already in your library as {name}"))),
                    Status::InBatch(name) => (RowStatus::Skip, Some(format!("Same file as {name} above"))),
                    Status::Unsupported => (RowStatus::Skip, Some(UNSUPPORTED.into())),
                    Status::Unreadable(why) => (RowStatus::Skip, Some(format!("Skipped: {why}"))),
                };
                let mut name = TextField::default();
                name.insert(&p.base);
                ImportRow {
                    source: p.cand.name.clone(),
                    path: p.cand.source.clone(),
                    size: p.cand.size,
                    kind: p.cand.kind,
                    status,
                    dup_of: None,
                    skip_reason,
                    unreadable: matches!(p.status, Status::Unreadable(_)),
                    name,
                    auto_renamed: p.auto_renamed,
                    error: None,
                    outcome: None,
                }
            })
            .collect();
        // Link each in-batch duplicate to the earlier row it repeats (same name and
        // size; the plan already found the content identical).
        for (i, p) in plan.rows.iter().enumerate() {
            if let Status::InBatch(name) = &p.status {
                self.rows[i].dup_of = (0..i).rev().find(|j| {
                    let r = &self.rows[*j];
                    r.status == RowStatus::Copy && r.source == *name && r.size == p.cand.size
                });
            }
        }
        self.stage = Stage::Review;
        self.scroll = 0.0;
        // Focus ← rather than the first name: a focused text box always shows its
        // ring, which the review screen design does not. Tab reaches the names next.
        self.focus = AddFocus::Back;
        self.validate(library);
    }

    /// Back to the start page (← or Choose again).
    pub fn reset(&mut self) {
        self.rows.clear();
        self.scroll = 0.0;
        self.stage = Stage::Start;
        self.focus = AddFocus::ChooseFiles;
        self.error = None;
        self.links.clear();
        self.link_errors.clear();
        self.from_download = false;
        self.link_failures = 0;
    }

    // ---- Download from Twitter ----

    /// Opens the links page, with one empty link to type or paste into.
    pub fn open_links(&mut self) {
        self.reset();
        self.stage = Stage::Links;
        self.add_link();
        self.focus_visible = false;
    }

    /// The review of downloaded files goes back to the links (kept as they were).
    pub fn back_to_links(&mut self) {
        self.rows.clear();
        self.scroll = 0.0;
        self.error = None;
        self.from_download = false;
        self.link_failures = 0;
        self.stage = Stage::Links;
        self.focus = AddFocus::Link(0);
        if self.links.is_empty() {
            self.add_link();
        }
    }

    pub fn links_editable(&self) -> bool {
        self.stage == Stage::Links
    }

    /// "Add another link": a new empty row at the end, focused.
    pub fn add_link(&mut self) {
        if self.links.len() < crate::twitter::MAX_LINKS {
            self.links.push(TextField::default());
            self.link_errors.push(None);
        }
        self.focus = AddFocus::Link(self.links.len() - 1);
    }

    /// The × beside a link. The last link is emptied rather than removed.
    pub fn remove_link(&mut self, i: usize) {
        if !self.links_editable() || i >= self.links.len() {
            return;
        }
        self.links.remove(i);
        self.link_errors.remove(i);
        if self.links.is_empty() {
            self.add_link();
        }
        self.focus = AddFocus::RemoveLink(i.min(self.links.len() - 1));
        self.error = None;
    }

    /// "Paste links": the first empty link takes the clipboard (a new one if
    /// none is empty). The host then pastes with `insert_links`.
    pub fn paste_target(&mut self) {
        match self.links.iter().position(TextField::is_empty) {
            Some(i) => self.focus = AddFocus::Link(i),
            None => self.add_link(),
        }
    }

    /// Typed or pasted text for link `i`. Several links (separated by spaces or
    /// line breaks) go one per row: the first into this link (or a new row after
    /// it when it already holds text), the others into new rows after it.
    pub fn insert_links(&mut self, i: usize, text: &str) {
        if !self.links_editable() || i >= self.links.len() {
            return;
        }
        // From pasted text with several words, only the post links are kept.
        let parts = crate::twitter::split_links(text);
        let posts: Vec<&String> = parts.iter().filter(|p| crate::twitter::post_id(p).is_some()).collect();
        if parts.len() <= 1 || posts.is_empty() {
            self.links[i].insert(text.trim());
            self.link_edited(i, text.chars().count() > 1);
            return;
        }
        let mut at = i;
        for (k, part) in posts.into_iter().enumerate() {
            let keep = k == 0 && (self.links[at].is_empty() || self.links[at].all_selected());
            if !keep {
                if self.links.len() >= crate::twitter::MAX_LINKS {
                    self.error = Some(format!("At most {} links at a time.", crate::twitter::MAX_LINKS));
                    break;
                }
                at += 1;
                self.links.insert(at, TextField::default());
                self.link_errors.insert(at, None);
            }
            self.links[at].insert(part);
            self.link_edited(at, true);
        }
        self.focus = AddFocus::Link(at);
    }

    /// Call after link `i` changes. A pasted link is checked at once; while
    /// typing, the message waits until focus leaves the link (`check_links`).
    pub fn link_edited(&mut self, i: usize, pasted: bool) {
        // The text changed, so an earlier message about it no longer applies.
        if let Some(e) = self.link_errors.get_mut(i) {
            *e = None;
        }
        if pasted {
            self.check_link(i);
        }
        self.error = None;
    }

    /// Shows or clears "not a link" for link `i` (a download's message stays).
    fn check_link(&mut self, i: usize) {
        if let (Some(f), Some(e)) = (self.links.get(i), self.link_errors.get_mut(i)) {
            let t = f.text().trim();
            if !t.is_empty() && crate::twitter::post_id(t).is_none() {
                *e = Some(NOT_A_LINK.into());
            } else if e.as_deref() == Some(NOT_A_LINK) {
                *e = None;
            }
        }
    }

    /// Says which links are not post links, except the one being typed in.
    pub fn check_links(&mut self) {
        for i in 0..self.links.len() {
            if self.focus != AddFocus::Link(i) {
                self.check_link(i);
            }
        }
    }

    /// The links to download: (row, post number), each post once.
    pub fn valid_links(&self) -> Vec<(usize, u64)> {
        let mut seen = HashSet::new();
        self.links
            .iter()
            .enumerate()
            .filter_map(|(i, f)| crate::twitter::post_id(f.text()).map(|id| (i, id)))
            .filter(|(_, id)| seen.insert(*id))
            .collect()
    }

    /// Download is possible: at least one post link and no text that is not one.
    pub fn can_download(&self) -> bool {
        self.stage == Stage::Links
            && !self.valid_links().is_empty()
            && self.links.iter().all(|f| f.text().trim().is_empty() || crate::twitter::post_id(f.text()).is_some())
    }

    /// Starts the download stage. Returns the links to download.
    pub fn begin_download(&mut self) -> Option<Vec<(usize, u64)>> {
        if !self.can_download() {
            return None;
        }
        self.link_errors.iter_mut().for_each(|e| *e = None);
        self.error = None;
        self.stage = Stage::Downloading { progress: None, stopping: false };
        self.focus = AddFocus::Cancel;
        Some(self.valid_links())
    }

    pub fn download_progress(&mut self, p: crate::download::Progress) {
        if let Stage::Downloading { progress, .. } = &mut self.stage {
            *progress = Some(p);
        }
    }

    /// Cancel while downloading: the worker stops and removes what it saved.
    pub fn stop_download(&mut self) {
        if let Stage::Downloading { stopping, .. } = &mut self.stage {
            *stopping = true;
        }
    }

    /// The download ended. `problems`: (link row, why) for links that gave
    /// nothing. With `files`, the files go on to the review; without, the links
    /// page shows why (and `message`, a reason for all of them).
    pub fn download_ended(&mut self, problems: &[(usize, String)], files: bool, message: Option<String>) {
        self.stage = Stage::Links;
        for (row, why) in problems {
            if let Some(e) = self.link_errors.get_mut(*row) {
                *e = Some(why.clone());
            }
        }
        self.error = message;
        self.focus = AddFocus::Link(problems.first().map_or(0, |p| p.0).min(self.links.len().saturating_sub(1)));
        self.from_download = files;
        self.link_failures = if files { problems.len() } else { 0 };
    }

    /// The line under the links while downloading.
    pub fn download_status(&self) -> Option<String> {
        let Stage::Downloading { progress, stopping } = self.stage else { return None };
        if stopping {
            return Some("Stopping…".into());
        }
        let Some(p) = progress else { return Some("Finding the posts…".into()) };
        let post = if p.posts > 1 { format!("Post {} of {} · ", p.post + 1, p.posts) } else { String::new() };
        let mb = |b: u64| format!("{:.1}", b as f64 / (1024.0 * 1024.0));
        Some(match (p.converting, p.done, p.total) {
            (true, ..) => format!("{post}Making the GIF…"),
            (_, 0, _) => format!("{post}Finding the media…"),
            (_, done, Some(total)) => format!("{post}Downloading {} of {} MB…", mb(done), mb(total)),
            (_, done, None) => format!("{post}Downloading {} MB…", mb(done)),
        })
    }

    /// How far the current video is, 0 to 1, when its size is known.
    pub fn download_fraction(&self) -> Option<f32> {
        match self.stage {
            Stage::Downloading { progress: Some(p), stopping: false } if !p.converting => {
                let total = p.total.filter(|t| *t > 0)?;
                let one = (p.done as f64 / total as f64).min(1.0);
                Some(((p.post as f64 + one) / p.posts.max(1) as f64) as f32)
            }
            _ => None,
        }
    }

    /// The × on a review row: leaves that file out of this import (the row goes).
    /// A later row that was skipped as the same file takes its place, with a free
    /// name, so that content is still added once. With no rows left, back to the start.
    pub fn remove(&mut self, i: usize, library: &[&str]) {
        if self.stage != Stage::Review || i >= self.rows.len() {
            return;
        }
        if self.rows[i].status == RowStatus::Copy {
            let dups: Vec<usize> = (i + 1..self.rows.len()).filter(|j| self.rows[*j].dup_of == Some(i)).collect();
            if let Some((&first, rest)) = dups.split_first() {
                let base = self.free_base(first, i, library);
                let row = &mut self.rows[first];
                row.status = RowStatus::Copy;
                row.skip_reason = None;
                row.dup_of = None;
                row.auto_renamed = base.1;
                row.name = TextField::default();
                row.name.insert(&base.0);
                let reason = format!("Same file as {} above", row.source);
                for &j in rest {
                    self.rows[j].dup_of = Some(first);
                    self.rows[j].skip_reason = Some(reason.clone());
                }
            }
        }
        self.rows.remove(i);
        for r in &mut self.rows {
            if let Some(d) = r.dup_of.as_mut()
                && *d > i
            {
                *d -= 1;
            }
        }
        if self.rows.is_empty() {
            self.reset();
            return;
        }
        self.focus = AddFocus::Remove(i.min(self.rows.len() - 1));
        self.validate(library);
    }

    /// Row `row`'s file name, or `name (2)`, `name (3)`… whichever is not taken by
    /// the library or another row being added (row `leaving` does not count).
    fn free_base(&self, row: usize, leaving: usize, library: &[&str]) -> (String, bool) {
        let r = &self.rows[row];
        let stem = crate::search::name_without_extension(&r.source).trim().to_string();
        let ext = r.ext().to_lowercase();
        let mut taken: HashSet<String> = library.iter().map(|n| n.to_lowercase()).collect();
        for (j, other) in self.rows.iter().enumerate() {
            if j != leaving && other.status == RowStatus::Copy {
                taken.insert(format!("{}{}", other.name.text().trim(), other.ext()).to_lowercase());
            }
        }
        let free = |b: &str| !taken.contains(&format!("{b}{ext}").to_lowercase());
        if free(&stem) {
            return (stem, false);
        }
        (2..1000).map(|n| format!("{stem} ({n})")).find(|b| free(b)).map_or((stem, false), |b| (b, true))
    }

    pub fn validate(&mut self, library: &[&str]) {
        let copy: Vec<usize> = (0..self.rows.len()).filter(|i| self.rows[*i].status == RowStatus::Copy).collect();
        let results = names::check_batch(copy.iter().map(|i| (self.rows[*i].name.text(), self.rows[*i].ext())), library);
        for (i, r) in copy.into_iter().zip(results) {
            self.rows[i].error = r;
        }
    }

    /// Call after the user edits row `i`.
    pub fn edited(&mut self, i: usize, library: &[&str]) {
        if let Some(row) = self.rows.get_mut(i) {
            row.auto_renamed = false;
        }
        self.validate(library);
    }

    pub fn ready(&self) -> usize {
        self.rows.iter().filter(|r| r.status == RowStatus::Copy).count()
    }

    pub fn duplicates(&self) -> usize {
        self.rows.iter().filter(|r| r.status == RowStatus::Skip && !r.unsupported() && !r.unreadable).count()
    }

    /// Files or folders that could not be read.
    pub fn unreadable(&self) -> usize {
        self.rows.iter().filter(|r| r.unreadable).count()
    }

    pub fn unsupported(&self) -> usize {
        self.rows.iter().filter(|r| r.unsupported()).count()
    }

    /// Add stays disabled while any row is invalid or nothing can be added.
    pub fn can_add(&self) -> bool {
        self.stage == Stage::Review && self.ready() > 0 && self.rows.iter().all(|r| r.error.is_none())
    }

    pub fn summary(&self) -> String {
        match self.stage {
            Stage::Scanning => "Looking at the files…".into(),
            Stage::Copying { stopping: true, .. } => "Stopping…".into(),
            Stage::Copying { done, total, .. } => format!("Adding {} of {total}…", (done + 1).min(total)),
            _ => {
                let mut text = format!("{} ready to add", self.ready());
                if self.link_failures > 0 {
                    text += &format!(" · {} could not be downloaded", plural(self.link_failures, "link"));
                }
                let d = self.duplicates();
                if d > 0 {
                    text += &format!(" · {} skipped", plural(d, "duplicate"));
                }
                let u = self.unsupported();
                if u > 0 {
                    text += &format!(" · {u} not supported");
                }
                let r = self.unreadable();
                if r > 0 {
                    text += &format!(" · {r} could not be read");
                }
                text
            }
        }
    }

    pub fn add_label(&self) -> String {
        match self.stage {
            Stage::Copying { .. } => "Adding…".into(),
            Stage::Downloading { .. } => "Downloading…".into(),
            Stage::Links => match self.valid_links().len() {
                0 | 1 => "Download".into(),
                n => format!("Download {n} links"),
            },
            _ => format!("Add {}", plural(self.ready(), "file")),
        }
    }

    /// The copies to make, with each row's index. Starts the copying stage.
    pub fn begin_copy(&mut self) -> Option<Vec<(usize, CopyRequest)>> {
        if !self.can_add() {
            return None;
        }
        let requests: Vec<(usize, CopyRequest)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.status == RowStatus::Copy)
            .filter_map(|(i, r)| {
                Some((
                    i,
                    CopyRequest {
                        source: r.path.clone(),
                        kind: r.kind?,
                        base: r.name.text().trim_start().to_string(),
                        ext: r.ext().to_string(),
                    },
                ))
            })
            .collect();
        self.stage = Stage::Copying { done: 0, total: requests.len(), stopping: false };
        self.focus = AddFocus::Cancel;
        Some(requests)
    }

    /// A copy finished (or failed).
    pub fn copied(&mut self, index: usize, result: &Result<Copied, String>) {
        if let Some(row) = self.rows.get_mut(index) {
            row.outcome = Some(result.as_ref().map(|c| c.name.clone()).map_err(String::clone));
        }
        if let Stage::Copying { done, .. } = &mut self.stage {
            *done += 1;
        }
    }

    /// Cancel while copying: the current copy is abandoned (its temporary file is
    /// removed); finished copies stay.
    pub fn stop(&mut self) {
        if let Stage::Copying { stopping, .. } = &mut self.stage {
            *stopping = true;
        }
    }

    /// All copies are done (or stopped). Returns the result line and goes back to the start.
    pub fn finish(&mut self, stopped: bool) -> String {
        let added = self.rows.iter().filter(|r| matches!(r.outcome, Some(Ok(_)))).count();
        let failed: Vec<&ImportRow> = self.rows.iter().filter(|r| matches!(r.outcome, Some(Err(_)))).collect();
        let mut text = format!("{} added", plural(added, "file"));
        let d = self.duplicates();
        if d > 0 {
            text += &format!(" · {} skipped", plural(d, "duplicate"));
        }
        let u = self.unsupported();
        if u > 0 {
            text += &format!(" · {u} not supported");
        }
        let r = self.unreadable();
        if r > 0 {
            text += &format!(" · {r} could not be read");
        }
        if let Some(first) = failed.first() {
            let why = match &first.outcome {
                Some(Err(e)) => e.as_str(),
                _ => "",
            };
            text += &format!(" · {} could not be added ({}: {why})", failed.len(), first.source);
        }
        if self.link_failures > 0 {
            text += &format!(" · {} could not be downloaded", plural(self.link_failures, "link"));
        }
        if stopped {
            text += " · Stopped before the rest";
        }
        if self.from_download {
            text += ".";
        } else {
            text += " · Originals left where they were.";
        }
        self.reset();
        text
    }

    pub fn focus_order(&self) -> Vec<AddFocus> {
        let mut order = vec![AddFocus::Close, AddFocus::Back];
        match self.stage {
            Stage::Start => order.extend([AddFocus::ChooseFiles, AddFocus::ImportExisting, AddFocus::DownloadTwitter]),
            Stage::Links => {
                for i in 0..self.links.len() {
                    order.extend([AddFocus::Link(i), AddFocus::RemoveLink(i)]);
                }
                if self.links.len() < crate::twitter::MAX_LINKS {
                    order.push(AddFocus::AddLink);
                }
                order.extend([AddFocus::PasteLinks, AddFocus::Cancel]);
                if self.can_download() {
                    order.push(AddFocus::Add);
                }
            }
            Stage::Review => {
                for i in 0..self.rows.len() {
                    if self.rows[i].status == RowStatus::Copy {
                        order.push(AddFocus::Row(i));
                    }
                    order.push(AddFocus::Remove(i));
                }
                order.extend([AddFocus::ChooseAgain, AddFocus::Cancel]);
                if self.can_add() {
                    order.push(AddFocus::Add);
                }
            }
            Stage::Scanning | Stage::Downloading { .. } | Stage::Copying { .. } => order.push(AddFocus::Cancel),
        }
        order
    }

    pub fn tab(&mut self, back: bool) {
        let order = self.focus_order();
        let cur = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if back { (cur + order.len() - 1) % order.len() } else { (cur + 1) % order.len() };
        self.focus = order[next];
        self.focus_visible = true;
        self.check_links();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{Candidate, Planned};

    const LIBRARY: [&str; 3] = ["Portrait.png", "Wave.gif", "Hello.gif"];

    fn row(name: &str, status: Status, base: &str, auto: bool) -> Planned {
        let cand =
            Candidate { source: PathBuf::from(format!("C:/src/{name}")), name: name.into(), size: 1, kind: crate::media_info::kind_for(name), problem: None };
        Planned { cand, status, base: base.into(), auto_renamed: auto }
    }

    fn review() -> AddMedia {
        let plan = Plan {
            rows: vec![
                row("Cat.png", Status::Copy, "Cat", false),
                row("Wave.gif", Status::Copy, "Wave (2)", true),
                row("Clip.mp4", Status::Copy, "Clip", false),
                row("IMG_4821.png", Status::Copy, "IMG_4821", false),
                row("Portrait.png", Status::InLibrary("Portrait.png".into()), "Portrait", false),
                row("notes.txt", Status::Unsupported, "notes", false),
            ],
            library_hashes: vec![],
        };
        let mut a = AddMedia::new();
        a.begin_scan();
        assert!(a.busy());
        a.load_plan(&plan, &LIBRARY);
        a
    }

    fn rename(a: &mut AddMedia, row: usize, to: &str) {
        a.rows[row].name.select_all();
        a.rows[row].name.backspace();
        a.rows[row].name.insert(to);
        a.edited(row, &LIBRARY);
    }

    /// An import saves the library file a few times, not once per file.
    #[test]
    fn an_import_saves_in_batches_not_per_file() {
        let t0 = Instant::now();
        let mut b = SaveBatch::default();
        let saves = (0..50).filter(|_| b.copied(t0)).count() + usize::from(b.finished());
        assert_eq!(saves, 1, "50 quick copies: one save at the end");
        let saves = (0..250).filter(|_| b.copied(t0)).count() + usize::from(b.finished());
        assert_eq!(saves, 3, "250 quick copies: after 100, after 200, and the last 50 at the end");
        assert!(!b.finished(), "nothing left to save");
        // Slow copies are saved after a while, so little is ever left unsaved.
        assert!(!b.copied(t0));
        assert!(b.copied(t0 + SaveBatch::AFTER));
        assert!(!b.finished());
    }

    /// A file that could not be read is its own kind of skipped row.
    #[test]
    fn unreadable_files_are_counted_on_their_own() {
        let mut a = AddMedia::new();
        let plan = Plan {
            rows: vec![
                row("Cat.png", Status::Copy, "Cat", false),
                row("Lost.png", Status::Unreadable("C:\\src\\Lost.png could not be read: denied".into()), "Lost", false),
                row("Folder", Status::Unreadable("the folder C:\\src\\Folder could not be opened: denied".into()), "Folder", false),
            ],
            library_hashes: Vec::new(),
        };
        a.load_plan(&plan, &[]);
        assert_eq!(a.summary(), "1 ready to add · 2 could not be read");
        assert_eq!((a.duplicates(), a.unsupported(), a.unreadable()), (0, 0, 2));
        assert_eq!(a.rows[1].message().as_deref(), Some("Skipped: C:\\src\\Lost.png could not be read: denied"));
        assert!(a.can_add());
    }

    #[test]
    fn the_plan_becomes_the_review_list() {
        let a = review();
        let names: Vec<&str> = a.rows.iter().map(|r| r.name.text()).collect();
        assert_eq!(names, ["Cat", "Wave (2)", "Clip", "IMG_4821", "Portrait", "notes"]);
        assert_eq!(a.summary(), "4 ready to add · 1 duplicate skipped · 1 not supported");
        assert_eq!(a.add_label(), "Add 4 files");
        assert_eq!(a.rows[2].destination(), "Videos");
        assert_eq!(a.rows[1].message().as_deref(), Some(AUTO_RENAMED));
        assert_eq!(a.rows[4].message().as_deref(), Some("Already in your library as Portrait.png"));
        assert_eq!(a.rows[5].message().as_deref(), Some(UNSUPPORTED));
        assert!(a.can_add());
    }

    #[test]
    fn owner_test_cases_disable_add() {
        let mut a = review();
        rename(&mut a, 0, "Cat/photo");
        assert_eq!(a.rows[0].error, Some(NameError::IllegalChar));
        assert!(!a.can_add());
        rename(&mut a, 0, "");
        assert_eq!(a.rows[0].error, Some(NameError::Empty));
        rename(&mut a, 0, "Portrait");
        assert_eq!(a.rows[0].error, Some(NameError::Taken), "a library name is taken");
        rename(&mut a, 0, "Wave");
        assert!(a.can_add(), ".png and .gif are different files");
        rename(&mut a, 3, "wave");
        assert_eq!(a.rows[3].error, Some(NameError::Taken), "same name and extension as an earlier row");
        rename(&mut a, 3, "Birthday confetti");
        assert!(a.can_add());
        rename(&mut a, 1, "Party");
        assert_eq!(a.rows[1].message(), None, "editing clears the automatic-number note");
    }

    #[test]
    fn copying_reports_progress_and_the_result() {
        let mut a = review();
        rename(&mut a, 0, "Cat photo");
        let reqs = a.begin_copy().unwrap();
        assert_eq!(reqs.iter().map(|(i, r)| (*i, format!("{}{}", r.base, r.ext))).collect::<Vec<_>>(), [
            (0, "Cat photo.png".to_string()),
            (1, "Wave (2).gif".into()),
            (2, "Clip.mp4".into()),
            (3, "IMG_4821.png".into())
        ]);
        assert!(!a.can_add(), "Add is off while copying");
        assert_eq!(a.summary(), "Adding 1 of 4…");
        let ok = |name: &str| Ok(Copied { name: name.into(), ..Default::default() });
        a.copied(0, &ok("Cat photo.png"));
        a.copied(1, &ok("Wave (2).gif"));
        a.copied(2, &Err("Clip.mp4 could not be read: denied".into()));
        assert_eq!(a.summary(), "Adding 4 of 4…");
        a.stop();
        assert_eq!(a.summary(), "Stopping…");
        let status = a.finish(true);
        assert_eq!(
            status,
            "2 files added · 1 duplicate skipped · 1 not supported · 1 could not be added (Clip.mp4: Clip.mp4 could not be read: denied) \
             · Stopped before the rest · Originals left where they were."
        );
        assert_eq!(a.stage, Stage::Start);
    }

    #[test]
    fn tab_skips_add_while_disabled_and_only_cancel_while_busy() {
        let mut a = review();
        rename(&mut a, 0, "");
        a.focus = AddFocus::Cancel;
        a.tab(false);
        assert_eq!(a.focus, AddFocus::Close);
        rename(&mut a, 0, "Cat");
        a.begin_copy();
        assert_eq!(a.focus_order(), [AddFocus::Close, AddFocus::Back, AddFocus::Cancel]);
    }

    #[test]
    fn the_x_leaves_a_file_out() {
        let mut a = review();
        a.remove(2, &LIBRARY); // Clip.mp4
        let names: Vec<&str> = a.rows.iter().map(|r| r.source.as_str()).collect();
        assert_eq!(names, ["Cat.png", "Wave.gif", "IMG_4821.png", "Portrait.png", "notes.txt"]);
        assert_eq!(a.summary(), "3 ready to add · 1 duplicate skipped · 1 not supported");
        assert_eq!(a.add_label(), "Add 3 files");
        let reqs = a.begin_copy().unwrap();
        assert!(reqs.iter().all(|(_, r)| r.ext != ".mp4"), "the removed file is not copied");
    }

    #[test]
    fn removing_a_bad_row_enables_add_and_skipped_rows_can_go_too() {
        let mut a = review();
        rename(&mut a, 3, "Cat"); // collides with row 0
        assert!(!a.can_add());
        a.remove(3, &LIBRARY);
        assert!(a.can_add(), "the collision left with the row");
        a.remove(4, &LIBRARY); // notes.txt (unsupported)
        a.remove(3, &LIBRARY); // Portrait.png (already in the library)
        assert_eq!(a.summary(), "3 ready to add");
        assert_eq!(a.focus, AddFocus::Remove(2), "focus stays on an × near the removed row");
    }

    #[test]
    fn removing_every_row_goes_back_to_the_start() {
        let mut a = review();
        while !a.rows.is_empty() {
            a.remove(0, &LIBRARY);
        }
        assert_eq!(a.stage, Stage::Start);
        assert!(!a.can_add());
    }

    #[test]
    fn removing_a_file_promotes_its_duplicate_with_a_free_name() {
        let big = |name: &str, status: Status, base: &str| {
            let mut r = row(name, status, base, false);
            r.cand.size = 7;
            r
        };
        let plan = Plan {
            rows: vec![
                big("Dog.png", Status::Copy, "Dog"),
                big("Cat.png", Status::Copy, "Cat"),
                // Another folder's Cat.png, same content: skipped as a duplicate.
                big("Cat.png", Status::InBatch("Cat.png".into()), "Cat"),
                big("Cat.png", Status::InBatch("Cat.png".into()), "Cat"),
            ],
            library_hashes: vec![],
        };
        let mut a = AddMedia::new();
        a.load_plan(&plan, &["Cat.png"]);
        assert_eq!(a.rows[2].dup_of, Some(1));
        assert_eq!(a.ready(), 2);
        a.remove(1, &["Cat.png"]);
        assert_eq!(a.ready(), 2, "the content is still added once");
        assert_eq!(a.rows[1].status, RowStatus::Copy);
        assert_eq!(a.rows[1].name.text(), "Cat (2)", "Cat.png is taken by the library");
        assert_eq!(a.rows[1].message().as_deref(), Some(AUTO_RENAMED));
        assert_eq!(a.rows[2].dup_of, Some(1), "the other copy now points at the promoted row");
        assert!(a.can_add());
        a.remove(2, &["Cat.png"]);
        assert_eq!(a.ready(), 2);
    }

    #[test]
    fn tab_reaches_each_rows_x() {
        let a = review();
        let order = a.focus_order();
        let pos = |f: AddFocus| order.iter().position(|o| *o == f).unwrap();
        assert!(pos(AddFocus::Row(0)) < pos(AddFocus::Remove(0)));
        assert!(pos(AddFocus::Remove(0)) < pos(AddFocus::Row(1)));
        assert!(order.contains(&AddFocus::Remove(5)), "skipped rows have an × too");
    }

    const A: &str = "https://twitter.com/name/status/111";
    const B: &str = "https://x.com/other/status/222?s=20";

    #[test]
    fn the_links_page_opens_with_one_empty_link() {
        let mut a = AddMedia::new();
        a.open_links();
        assert_eq!(a.stage, Stage::Links);
        assert!(a.links_page() && !a.reviewing() && !a.busy());
        assert_eq!(a.links.len(), 1);
        assert_eq!(a.focus, AddFocus::Link(0));
        assert!(!a.can_download());
        assert_eq!(a.add_label(), "Download");
    }

    #[test]
    fn pasting_several_links_fills_one_row_each_and_keeps_only_links() {
        let mut a = AddMedia::new();
        a.open_links();
        a.insert_links(0, &format!("look at this {A}\r\n and {B} too"));
        assert_eq!(a.links.iter().map(|f| f.text()).collect::<Vec<_>>(), vec![A, B]);
        assert_eq!(a.focus, AddFocus::Link(1));
        assert!(a.can_download());
        assert_eq!(a.add_label(), "Download 2 links");
        // Into a link that already has text: the new ones go after it.
        a.focus = AddFocus::Link(0);
        a.insert_links(0, &format!("{B} {A}"));
        assert_eq!(a.links.len(), 4);
        assert_eq!(a.links[1].text(), B);
        // The same post twice is downloaded once.
        assert_eq!(a.valid_links(), vec![(0, 111), (1, 222)]);
    }

    #[test]
    fn text_without_a_link_stays_in_one_row_and_is_flagged() {
        let mut a = AddMedia::new();
        a.open_links();
        a.insert_links(0, "hello there");
        assert_eq!(a.links.len(), 1);
        assert_eq!(a.link_errors[0].as_deref(), Some(NOT_A_LINK));
        assert!(!a.can_download());
    }

    #[test]
    fn a_typed_link_is_checked_when_focus_leaves_it() {
        let mut a = AddMedia::new();
        a.open_links();
        a.insert_links(0, "h");
        assert_eq!(a.link_errors[0], None, "no message while typing");
        a.tab(false);
        assert_eq!(a.link_errors[0].as_deref(), Some(NOT_A_LINK));
        a.focus = AddFocus::Link(0);
        a.links[0].select_all();
        a.insert_links(0, A);
        assert_eq!(a.link_errors[0], None);
        assert!(a.can_download());
    }

    #[test]
    fn at_most_twenty_links() {
        let mut a = AddMedia::new();
        a.open_links();
        let many: Vec<String> = (1..=25).map(|n| format!("https://x.com/a/status/{n}")).collect();
        a.insert_links(0, &many.join("\n"));
        assert_eq!(a.links.len(), crate::twitter::MAX_LINKS);
        assert!(a.error.is_some());
        a.add_link();
        assert_eq!(a.links.len(), crate::twitter::MAX_LINKS);
        assert!(!a.focus_order().contains(&AddFocus::AddLink));
    }

    #[test]
    fn removing_the_last_link_leaves_an_empty_one() {
        let mut a = AddMedia::new();
        a.open_links();
        a.insert_links(0, A);
        a.remove_link(0);
        assert_eq!(a.links.len(), 1);
        assert!(a.links[0].is_empty());
    }

    #[test]
    fn a_download_reports_progress_and_problems_per_link() {
        let mut a = AddMedia::new();
        a.open_links();
        a.insert_links(0, &format!("{A} {B}"));
        assert_eq!(a.begin_download(), Some(vec![(0, 111), (1, 222)]));
        assert!(a.busy() && a.links_page());
        assert_eq!(a.focus_order(), vec![AddFocus::Close, AddFocus::Back, AddFocus::Cancel]);
        assert_eq!(a.download_status().as_deref(), Some("Finding the posts…"));
        a.download_progress(crate::download::Progress { post: 1, posts: 2, done: 1_048_576, total: Some(4_194_304), converting: false });
        assert_eq!(a.download_status().as_deref(), Some("Post 2 of 2 · Downloading 1.0 of 4.0 MB…"));
        assert_eq!(a.download_fraction(), Some(0.625));
        a.stop_download();
        assert_eq!(a.download_status().as_deref(), Some("Stopping…"));
        // One link gave nothing: the other's files go on to the review.
        a.download_ended(&[(1, crate::twitter::NOTHING.into())], true, None);
        assert_eq!(a.stage, Stage::Links);
        assert_eq!(a.link_errors[1].as_deref(), Some(crate::twitter::NOTHING));
        assert!(a.from_download);
        assert_eq!(a.link_failures, 1);
    }

    #[test]
    fn the_review_of_downloads_goes_back_to_the_links_and_says_nothing_about_originals() {
        let mut a = review();
        a.links.push(TextField::default());
        a.links[0].insert(A);
        a.link_errors.push(None);
        a.from_download = true;
        a.link_failures = 1;
        assert!(a.summary().contains("1 link could not be downloaded"), "{}", a.summary());
        a.back_to_links();
        assert_eq!(a.stage, Stage::Links);
        assert_eq!(a.links[0].text(), A);
        assert!(!a.from_download && a.rows.is_empty());

        let mut b = review();
        b.from_download = true;
        b.begin_copy().unwrap();
        let text = b.finish(false);
        assert!(!text.contains("Originals"), "{text}");
        assert!(b.links.is_empty() && !b.from_download, "the start page again");
    }
}
