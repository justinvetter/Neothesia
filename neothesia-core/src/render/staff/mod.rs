use wgpu_jumpstart::Color;

use crate::{
    render::{QuadInstance, QuadRenderer, TextRenderer, text::TextArea},
    utils::{Point, Size},
};

/// Diatonic step (C=0 .. B=6) within an octave and whether the pitch needs a sharp sign
const STEPS: [(u8, bool); 12] = [
    (0, false),
    (0, true),
    (1, false),
    (1, true),
    (2, false),
    (3, false),
    (3, true),
    (4, false),
    (4, true),
    (5, false),
    (5, true),
    (6, false),
];

/// Diatonic position of E4, the bottom line of the treble staff
const TREBLE_BOTTOM: i32 = 30;
/// Diatonic position of G2, the bottom line of the bass staff
const BASS_BOTTOM: i32 = 18;
/// Diatonic position of middle C
const MIDDLE_C: i32 = 28;

/// Highest and lowest diatonic positions with a line or ledger line we make room for
const TOP_LINE: i32 = 40;
const BOTTOM_LINE: i32 = 16;

/// Notes outside of these are folded back by whole octaves and marked with 8va/8vb
const TOP_EDGE: i32 = 42;
const BOTTOM_EDGE: i32 = 14;

/// Extra room (in staff spaces) between the treble and bass staves, so the clefs do not touch
const STAVE_GAP: f32 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StaffNote {
    pub midi: u8,
    /// Linear RGBA, as taken by [`QuadInstance`]
    pub color: [f32; 4],
}

struct Placed {
    diatonic: i32,
    sharp: bool,
    color: [f32; 4],
    /// Notehead is nudged to the right, so it does not overlap a neighbouring second
    shifted: bool,
}

/// Grand staff (treble + bass clef) that shows currently sounding notes
pub struct StaffRenderer {
    pos: Point<f32>,
    size: Size<f32>,

    notes: Vec<StaffNote>,
    /// Glyphs are static for a given layout, so they are shaped once and reused every frame
    text_cache: Vec<TextArea>,
}

fn text_area(
    text: &str,
    family: glyphon::Family,
    font_size: f32,
    x: f32,
    y: f32,
    color: glyphon::Color,
) -> TextArea {
    let font_system = crate::font_system::font_system();
    let font_system = &mut font_system.borrow_mut();

    let mut buffer = glyphon::Buffer::new(font_system, glyphon::Metrics::new(font_size, font_size));
    buffer.set_size(Some(font_size * 2.0), Some(font_size * 2.0));
    buffer.set_wrap(glyphon::Wrap::None);
    buffer.set_text(
        text,
        &glyphon::Attrs::new().family(family),
        glyphon::Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, false);

    TextArea {
        buffer,
        left: x,
        top: y,
        scale: 1.0,
        bounds: glyphon::TextBounds::default(),
        default_color: color,
    }
}

impl StaffRenderer {
    pub fn new() -> Self {
        Self {
            pos: Default::default(),
            size: Size::new(0.0, 0.0),
            notes: Vec::new(),
            text_cache: Vec::new(),
        }
    }

    pub fn set_rect(&mut self, pos: Point<f32>, size: Size<f32>) {
        if self.pos != pos || self.size != size {
            self.pos = pos;
            self.size = size;
            self.text_cache.clear();
        }
    }

    pub fn set_notes(&mut self, notes: &[StaffNote]) {
        if self.notes != notes {
            self.notes = notes.to_vec();
            self.notes.sort_by_key(|n| n.midi);
            self.text_cache.clear();
        }
    }

    /// Distance between two neighbouring staff lines
    fn space(&self) -> f32 {
        self.size.height / 18.0
    }

    /// Y coordinate of the line (or the centre of a space) at the given diatonic position
    fn y_of(&self, diatonic: i32) -> f32 {
        let s = self.space();
        let gap = match diatonic {
            d if d >= MIDDLE_C + 1 => 0.0,
            MIDDLE_C => STAVE_GAP / 2.0,
            _ => STAVE_GAP,
        };
        self.pos.y + s + (TOP_EDGE - diatonic) as f32 * s / 2.0 + gap * s
    }

    /// Returns the placed notes, plus how many octaves the highest and lowest notes were folded by
    fn place(&self) -> (Vec<Placed>, i32, i32) {
        let mut placed: Vec<Placed> = Vec::with_capacity(self.notes.len());
        let (mut fold_up, mut fold_down) = (0, 0);

        for note in &self.notes {
            let (step, sharp) = STEPS[(note.midi % 12) as usize];
            let octave = (note.midi / 12) as i32 - 1;
            let mut diatonic = octave * 7 + step as i32;

            let mut folds = 0;
            while diatonic > TOP_EDGE {
                diatonic -= 7;
                folds += 1;
            }
            fold_up = fold_up.max(folds);

            folds = 0;
            while diatonic < BOTTOM_EDGE {
                diatonic += 7;
                folds += 1;
            }
            fold_down = fold_down.max(folds);

            placed.push(Placed {
                diatonic,
                sharp,
                color: note.color,
                shifted: false,
            });
        }

        placed.sort_by_key(|p| p.diatonic);
        for i in 1..placed.len() {
            placed[i].shifted =
                placed[i].diatonic - placed[i - 1].diatonic <= 1 && !placed[i - 1].shifted;
        }

        (placed, fold_up, fold_down)
    }

    fn rebuild_text_cache(&mut self) {
        let s = self.space();
        let ink = glyphon::Color::rgb(20, 20, 25);

        let x = self.pos.x + s * 0.8;
        let music = glyphon::Family::Name("Noto Music");

        // The clef glyphs are positioned relative to the line they wrap around:
        // G clef curls around the G line, F clef sits on the F line
        let treble_line_y = self.y_of(TREBLE_BOTTOM + 2);
        let bass_line_y = self.y_of(BASS_BOTTOM + 6);
        let treble_size = s * 3.4;
        let bass_size = s * 3.0;
        self.text_cache.push(text_area(
            "\u{1D11E}",
            music,
            treble_size,
            x,
            treble_line_y - treble_size * 0.62,
            ink,
        ));
        self.text_cache.push(text_area(
            "\u{1D122}",
            music,
            bass_size,
            x,
            bass_line_y - bass_size * 0.5,
            ink,
        ));

        let note_x = self.note_x();
        let head_w = s * 1.3;
        let (placed, fold_up, fold_down) = self.place();
        for p in placed.iter().filter(|p| p.sharp) {
            let size = s * 2.2;
            self.text_cache.push(text_area(
                "\u{266F}",
                glyphon::Family::SansSerif,
                size,
                note_x - head_w * 0.5 - size * 0.5,
                self.y_of(p.diatonic) - size * 0.62,
                ink,
            ));
        }

        let label_size = s * 1.1;
        let label_x = note_x + head_w * 2.5;
        if fold_up > 0 {
            self.text_cache.push(text_area(
                Self::ottava_label(fold_up, true),
                glyphon::Family::SansSerif,
                label_size,
                label_x,
                self.pos.y + s * 0.1,
                ink,
            ));
        }
        if fold_down > 0 {
            self.text_cache.push(text_area(
                Self::ottava_label(fold_down, false),
                glyphon::Family::SansSerif,
                label_size,
                label_x,
                self.pos.y + self.size.height - s * 1.5,
                ink,
            ));
        }
    }

    fn ottava_label(folds: i32, up: bool) -> &'static str {
        match (folds, up) {
            (1, true) => "8va",
            (2, true) => "15ma",
            (_, true) => "22ma",
            (1, false) => "8vb",
            (2, false) => "15mb",
            (_, false) => "22mb",
        }
    }

    /// Horizontal centre of the notehead column
    fn note_x(&self) -> f32 {
        self.pos.x + self.size.width * 0.55
    }

    pub fn update(&mut self, quads: &mut QuadRenderer, text: &mut TextRenderer) {
        if self.size.width <= 0.0 || self.size.height <= 0.0 {
            return;
        }

        if self.text_cache.is_empty() {
            self.rebuild_text_cache();
        }

        let s = self.space();
        let line_h = (s * 0.09).max(1.0);
        let left = self.pos.x;
        let width = self.size.width;
        let ink = [0.08, 0.08, 0.1, 1.0];

        // Paper
        quads.push(QuadInstance {
            position: self.pos.into(),
            size: self.size.into(),
            color: Color::new(0.97, 0.96, 0.92, 0.96).into_linear_rgba(),
            border_radius: [s * 0.5; 4],
            ..Default::default()
        });

        let line = |quads: &mut QuadRenderer, y: f32, x: f32, w: f32| {
            quads.push(QuadInstance {
                position: [x, y - line_h / 2.0],
                size: [w, line_h],
                color: Color::new(ink[0], ink[1], ink[2], 1.0).into_linear_rgba(),
                ..Default::default()
            });
        };

        let margin = s * 0.5;
        for bottom in [TREBLE_BOTTOM, BASS_BOTTOM] {
            for i in 0..5 {
                line(quads, self.y_of(bottom + i * 2), left + margin, width - margin * 2.0);
            }
        }

        let note_x = self.note_x();
        let head_w = s * 1.3;
        let head_h = s * 0.95;
        let ledger_w = head_w * 1.9;

        for p in self.place().0 {
            let d = p.diatonic;
            let cx = note_x + if p.shifted { head_w * 0.95 } else { 0.0 };

            // Ledger lines
            let ledgers: Vec<i32> = if d == MIDDLE_C {
                vec![MIDDLE_C]
            } else if d > TOP_LINE - 1 {
                (TOP_LINE..=d).step_by(2).collect()
            } else if d < BOTTOM_LINE + 1 {
                (d..=BOTTOM_LINE).filter(|l| l % 2 == 0).collect()
            } else {
                Vec::new()
            };
            for l in ledgers {
                line(quads, self.y_of(l), cx - ledger_w / 2.0, ledger_w);
            }

            quads.push(QuadInstance {
                position: [cx - head_w / 2.0, self.y_of(d) - head_h / 2.0],
                size: [head_w, head_h],
                color: p.color,
                border_radius: [head_h / 2.0; 4],
                border_color: [0.08, 0.08, 0.1, 1.0],
                border_width: (s * 0.08).max(1.0),
            });
        }

        text.queue_mut().extend_from_slice(&self.text_cache);
    }
}
