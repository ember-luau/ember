/*! The live display of `embr install`, laid out after pesde's.

```text
    install ⣾ [00:00:03] ━━━━━━━━━━━━━━━━━━╸─────────── 12/20
    - roblox/roact@1.4.4  1.2 MiB / 3.4 MiB
    - sleitnick/knit@1.7.0
```

one run line: the phase, a spinner, the clock, and the bar. the clock starts
once and runs across every phase, so it reads as the whole install, not the
current step. under it, one dim row per package in flight, with its bytes
once the download knows them. a row leaves when its package is done.

off a terminal nothing draws, so each finished package prints its ✓ line
instead and a CI log still says what happened.
*/

use std::cell::Cell;
use std::time::Duration;

use crossterm::style::Stylize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use crate::ui;

/// pesde's spinner frames, a braille wheel.
const TICKS: &str = "⣷⣯⣟⡿⢿⣻⣽⣾";

pub struct Progress {
    multi: MultiProgress,
    run: ProgressBar,
}

impl Progress {
    /// starts the run line on `phase`, a spinner and the clock, no bar yet.
    pub fn start(phase: &str) -> Self {
        let multi = MultiProgress::new();
        let run = multi.add(ProgressBar::new_spinner());
        run.set_style(run_style(false));
        run.set_message(phase.to_string());
        run.enable_steady_tick(Duration::from_millis(100));
        Self { multi, run }
    }

    /// draws nothing and prints the off-terminal lines, for tests.
    #[cfg(test)]
    pub fn hidden() -> Self {
        let multi = MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let run = multi.add(ProgressBar::new_spinner());
        Self { multi, run }
    }

    /** moves the run line to `phase` with a bar of `len` steps. the clock
    keeps running, so the time at the left stays the whole install. */
    pub fn phase(&self, phase: &str, len: u64) {
        /* the name first and the bar last. each setter can draw a frame,
        and the other order showed the new count under the old name */
        self.run.set_message(phase.to_string());
        self.run.set_length(len);
        self.run.set_position(0);
        self.run.set_style(run_style(true));
    }

    /// false off a terminal, where nothing draws and finished work prints a line.
    pub fn is_live(&self) -> bool {
        !self.multi.is_hidden()
    }

    /// a row for one package or tool in flight, `label` as its name.
    pub fn task(&self, label: String) -> Task {
        let row = self.multi.add(ProgressBar::new(0));
        row.set_style(row_style(Size::Unknown));
        row.set_message(label);
        Task {
            row,
            multi: self.multi.clone(),
            run: self.run.clone(),
            size: Cell::new(Size::Unknown),
        }
    }

    /// a line on stderr the live display steps around.
    pub fn warn(&self, line: &str) {
        // warnings name package files and carry registry text
        self.multi.suspend(|| eprintln!("{}", ui::printable(line)));
    }

    /// runs `work` with the display out of the way, for code that prints itself.
    pub fn suspend<T>(&self, work: impl FnOnce() -> T) -> T {
        self.multi.suspend(work)
    }
}

/// the display comes down with its owner, an error return included.
impl Drop for Progress {
    fn drop(&mut self) {
        self.run.finish_and_clear();
        let _ = self.multi.clear();
    }
}

/// one row of the display. it leaves when the task drops, done or failed.
pub struct Task {
    row: ProgressBar,
    multi: MultiProgress,
    run: ProgressBar,
    /// what the row shows of the size, so its style changes once and not per chunk
    size: Cell<Size>,
}

impl Task {
    /// bytes so far, and the whole size when the server sent one.
    pub fn bytes(&self, done: u64, total: Option<u64>) {
        let size = match total {
            Some(_) => Size::Known,
            None => Size::Open,
        };
        if self.size.get() != size {
            self.row.set_style(row_style(size));
            self.size.set(size);
        }
        if let Some(total) = total {
            self.row.set_length(total);
        }
        self.row.set_position(done);
    }

    /** the task finished. the run bar steps, and off a terminal `line`
    prints in place of the row nobody sees. */
    pub fn done(self, line: &str) {
        if self.multi.is_hidden() {
            println!("{line}");
        }
        self.run.inc(1);
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.row.finish_and_clear();
        self.multi.remove(&self.row);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Size {
    /// nothing arrived yet
    Unknown,
    /// arriving, no Content-Length
    Open,
    /// arriving, and the whole size is known
    Known,
}

/** `phase ⣾ [00:00:03] ━━━━╸──── 12/20`, or without the bar while the
length is unknown. indicatif templates only speak ANSI 16 and 256, so the
exact palette gets smuggled in as crossterm escapes around the
placeholders, the same trick `ui` used for the old bar. */
fn run_style(with_bar: bool) -> ProgressStyle {
    let head = format!(
        "{} {} {}",
        "{msg:>11}".with(ui::accent()).bold(),
        "{spinner}".with(ui::accent()),
        "[{elapsed_precise}]".dark_grey(),
    );
    let template = match with_bar {
        true => format!("{head} {} {{pos}}/{{len}}", "{bar:30}".with(ui::accent())),
        false => head,
    };
    ProgressStyle::with_template(&template)
        .expect("run line template is valid")
        .tick_chars(TICKS)
        .progress_chars("━╸─")
}

/// `    - name@version  1.2 MiB / 3.4 MiB`, dim, sized as far as the download knows.
fn row_style(size: Size) -> ProgressStyle {
    let row = match size {
        Size::Unknown => "- {msg}",
        Size::Open => "- {msg}  {bytes}",
        Size::Known => "- {msg}  {bytes} / {total_bytes}",
    };
    ProgressStyle::with_template(&format!("            {}", row.dark_grey()))
        .expect("row template is valid")
}
