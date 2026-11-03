use std::time::Duration;

use midi_file::midly::MidiMessage;
use neothesia_core::render::{
    GlowRenderer, GuidelineRenderer, LiveWaterfallRenderer, QuadRenderer, StaffNote, StaffRenderer,
    TextRenderer,
};
use winit::{
    event::WindowEvent,
    keyboard::{Key, NamedKey},
};

use crate::{
    NeothesiaEvent,
    context::Context,
    scene::{
        MouseToMidiEventState, NuonRenderer, Scene,
        freeplay::recorder::{FreeplayRecorder, Preview, RecorderStatus},
        playing_scene::Keyboard,
    },
    song::Song,
    utils::{BoxFuture, noop_waker_ref, window::WinitEvent},
};

mod recorder;

type MsgFn = Box<dyn FnOnce(&mut FreeplayScene, &mut Context)>;

fn on_async<T, Fut, FN>(future: Fut, f: FN) -> BoxFuture<MsgFn>
where
    T: 'static,
    Fut: Future<Output = T> + Send + 'static,
    FN: FnOnce(T, &mut FreeplayScene, &mut Context) + Send + 'static,
{
    Box::pin(async {
        let res = future.await;
        let f: MsgFn = Box::new(move |state, ctx| f(res, state, ctx));
        f
    })
}

pub struct FreeplayScene {
    keyboard: Keyboard,
    guidelines: GuidelineRenderer,

    text_renderer: TextRenderer,
    quad_renderer_bg: QuadRenderer,
    quad_renderer_fg: QuadRenderer,
    glow: Option<GlowRenderer>,
    live_waterfall: LiveWaterfallRenderer,
    staff: StaffRenderer,
    show_staff: bool,

    // TODO: This does not make sens, but get's us going without refactoring
    song: Option<Song>,

    nuon_renderer: NuonRenderer,
    nuon: nuon::Ui,
    mouse_to_midi_state: MouseToMidiEventState,
    note_names: String,
    chord_name: String,

    recorder: FreeplayRecorder,
    recorder_status: RecorderStatus,
    preview: Option<Preview>,

    context: std::task::Context<'static>,
    futures: Vec<BoxFuture<MsgFn>>,
}

impl FreeplayScene {
    pub fn new(ctx: &mut Context, song: Option<Song>) -> Self {
        let mut keyboard = Keyboard::new(ctx, Default::default());
        keyboard.set_pressed_by_user_colors(ctx.config.color_schema()[0].clone());

        let keyboard_layout = keyboard.layout();

        let guidelines = GuidelineRenderer::new(
            keyboard_layout.clone(),
            *keyboard.pos(),
            ctx.config.vertical_guidelines(),
            false,
            Default::default(),
        );

        let text_renderer = ctx.text_renderer_factory.new_renderer();

        let quad_renderer_bg = ctx.quad_renderer_factory.new_renderer();
        let quad_renderer_fg = ctx.quad_renderer_factory.new_renderer();

        let glow = ctx.config.glow().then_some(GlowRenderer::new(
            &ctx.gpu,
            &ctx.transform,
            keyboard.layout(),
        ));

        let live_waterfall = LiveWaterfallRenderer::new(
            &ctx.gpu,
            &ctx.config,
            &ctx.transform,
            keyboard.layout().clone(),
        );

        Self {
            keyboard,
            guidelines,
            text_renderer,
            quad_renderer_bg,
            quad_renderer_fg,
            glow,
            live_waterfall,
            staff: StaffRenderer::new(),
            show_staff: true,
            song,
            nuon_renderer: NuonRenderer::new(ctx),
            nuon: nuon::Ui::new(),
            mouse_to_midi_state: MouseToMidiEventState::default(),
            note_names: String::new(),
            chord_name: String::new(),
            recorder: FreeplayRecorder::default(),
            recorder_status: RecorderStatus::default(),
            preview: None,

            context: std::task::Context::from_waker(noop_waker_ref()),
            futures: Vec::new(),
        }
    }

    fn update_glow(&mut self, delta: Duration) {
        let Some(glow) = &mut self.glow else {
            return;
        };

        glow.clear();

        let keys = &self.keyboard.layout().keys;
        let states = self.keyboard.key_states();

        for (key, state) in keys.iter().zip(states) {
            let Some(mut color) = state.pressed_by_user().copied() else {
                continue;
            };

            color.r *= 0.5;
            color.g *= 0.5;
            color.b *= 0.5;

            glow.push(
                key.id(),
                color,
                key.x(),
                self.keyboard.pos().y,
                key.width(),
                delta,
            );
        }
    }

    /// Names of the keys the user is holding, lowest first, plus the chord they form (if any)
    fn update_note_labels(&mut self) {
        let start = self.keyboard.layout().range.start();
        let notes: Vec<u8> = self
            .keyboard
            .key_states()
            .iter()
            .enumerate()
            .filter(|(_, state)| state.pressed_by_user().is_some())
            .map(|(id, _)| id as u8 + start)
            .collect();

        // Pitch names only (C, C♯, ...), each listed once even when played in several octaves
        let mut names: Vec<&str> = Vec::new();
        for &n in &notes {
            let name = chords::note_name(n);
            if !names.contains(&name) {
                names.push(name);
            }
        }
        self.note_names = names.join(" ");
        self.chord_name = if notes.len() >= 2 {
            chords::deduce_name(&notes).unwrap_or_default()
        } else {
            String::new()
        };
    }

    fn update_staff(&mut self, ctx: &Context) {
        if !self.show_staff {
            return;
        }

        let start = self.keyboard.layout().range.start();
        let notes: Vec<StaffNote> = self
            .keyboard
            .key_states()
            .iter()
            .enumerate()
            .filter_map(|(id, state)| {
                let color = state
                    .pressed_by_user()
                    .or_else(|| state.sustained_by_user())?;
                Some(StaffNote {
                    midi: id as u8 + start,
                    color: color.into_linear_rgba(),
                })
            })
            .collect();
        self.staff.set_notes(&notes);
        self.staff.set_caption(&self.chord_name, &self.note_names);

        let window = ctx.window_state.logical_size;
        let width = (window.width * 0.55).min(900.0);
        let height = (window.height * 0.45).min(width * 0.6);
        let x = (window.width - width) / 2.0;
        let y = self.keyboard.pos().y - 50.0 - height;
        self.staff
            .set_rect((x, y).into(), (width, height).into());
        self.staff
            .update(&mut self.quad_renderer_fg, &mut self.text_renderer);
    }

    fn update_ui(&mut self, ctx: &mut Context) {
        recorder::update_preview_ui(self, ctx);

        let width = ctx.window_state.logical_size.width;
        let names_y = self.keyboard.pos().y - 25.0 - 10.0;

        // Chord on top, the individual note names right above the keyboard
        nuon::label()
            .text(&self.chord_name)
            .font_size(36.0)
            .y(names_y - 40.0)
            .height(36.0)
            .width(width)
            .build(&mut self.nuon);

        nuon::label()
            .text(&self.note_names)
            .font_size(25.0)
            .y(names_y)
            .height(25.0)
            .width(width)
            .build(&mut self.nuon);
    }

    fn resize(&mut self, ctx: &mut Context) {
        self.keyboard.resize(ctx);
        self.guidelines.set_layout(self.keyboard.layout().clone());
        self.guidelines.set_pos(*self.keyboard.pos());
        self.live_waterfall
            .resize(&ctx.config, self.keyboard.layout().clone());
        if let Some(preview) = self.preview.as_mut() {
            preview.resize(&self.keyboard, ctx);
        }
    }

    fn dispatch_futures(&mut self, ctx: &mut Context) {
        let mut cbs = Vec::new();
        self.futures
            .retain_mut(|f| match f.as_mut().poll(&mut self.context) {
                std::task::Poll::Ready(msg) => {
                    cbs.push(msg);
                    false
                }
                std::task::Poll::Pending => true,
            });

        for cb in cbs {
            cb(self, ctx);
        }
    }
}

impl Scene for FreeplayScene {
    fn update(&mut self, ctx: &mut Context, delta: Duration) {
        self.quad_renderer_bg.clear();
        self.quad_renderer_fg.clear();

        self.dispatch_futures(ctx);

        if let Some(preview) = self.preview.as_mut() {
            preview.update(&mut self.keyboard, ctx, delta);
        }

        let time = 0.0;

        self.guidelines.update(
            &mut self.quad_renderer_bg,
            ctx.config.animation_speed(),
            ctx.window_state.scale_factor as f32,
            time,
            ctx.window_state.logical_size,
        );
        self.keyboard
            .update(&mut self.quad_renderer_fg, &mut self.text_renderer);

        self.update_glow(delta);
        self.update_note_labels();
        self.update_staff(ctx);

        self.live_waterfall.update(
            &ctx.config,
            delta,
            ctx.window_state.physical_size.height as f32,
        );

        self.quad_renderer_bg.prepare();
        self.quad_renderer_fg.prepare();

        if let Some(glow) = &mut self.glow {
            glow.prepare();
        }

        self.text_renderer.update(
            ctx.window_state.physical_size,
            ctx.window_state.scale_factor as f32,
        );

        self.update_ui(ctx);

        super::render_nuon(&mut self.nuon, &mut self.nuon_renderer, ctx);
    }

    fn render<'pass>(&'pass mut self, rpass: &mut wgpu_jumpstart::RenderPass<'pass>) {
        self.quad_renderer_bg.render(rpass);
        if let Some(preview) = self.preview.as_mut() {
            preview.render(rpass);
        } else {
            self.live_waterfall.render(rpass);
        }
        self.quad_renderer_fg.render(rpass);
        if let Some(glow) = &self.glow {
            glow.render(rpass);
        }
        self.text_renderer.render(rpass);
        self.nuon_renderer.render(rpass);
    }

    fn window_event(&mut self, ctx: &mut Context, event: &WindowEvent) {
        if event.window_resized() || event.scale_factor_changed() {
            self.resize(ctx)
        }

        if event.back_mouse_pressed() || event.key_released(Key::Named(NamedKey::Escape)) {
            ctx.proxy
                .send_event(NeothesiaEvent::MainMenu(self.song.clone()))
                .ok();
        }

        if event.key_released(Key::Named(NamedKey::F2)) {
            self.show_staff = !self.show_staff;
        }

        if event.key_released(Key::Named(NamedKey::Space)) && self.preview.is_some() {
            recorder::toggle_preview_playback(self);
        }

        super::handle_nuon_window_event(&mut self.nuon, event, ctx);
        super::handle_pc_keyboard_to_midi_event(ctx, event);
        super::handle_mouse_to_midi_event(
            &mut self.keyboard,
            &mut self.mouse_to_midi_state,
            ctx,
            event,
        );
    }

    fn midi_event(&mut self, ctx: &mut Context, channel: u8, message: &MidiMessage) {
        self.recorder.push_event(channel, *message);
        self.keyboard.user_midi_event(message);
        match message {
            MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                self.live_waterfall
                    .note_on(channel, key.as_int(), vel.as_int());
            }
            MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                self.live_waterfall.note_off(key.as_int());
            }
            _ => {}
        }
        ctx.output_manager
            .connection()
            .midi_event(channel.into(), *message);
    }
}

pub(crate) mod chords {
    /// Get chord name based on notes, eg. Cmaj7
    pub fn deduce_name(midi_notes: &[u8]) -> Option<String> {
        if midi_notes.is_empty() {
            return None;
        }

        if midi_notes.len() == 1 {
            return Some(note_name(midi_notes[0]).to_string());
        }

        // Normalize notes to a single octave and sort
        let mut normalized: Vec<u8> = midi_notes.iter().map(|&n| n % 12).collect();
        normalized.sort_unstable();
        normalized.dedup();

        if normalized.is_empty() {
            return None;
        }

        // The lowest note played, so inversions can be written as slash chords (C/E)
        let bass = midi_notes.iter().copied().min().unwrap_or(0) % 12;

        // Try the bass note as the root first, then every other note
        let mut candidates = normalized.clone();
        candidates.sort_by_key(|&n| n != bass);

        for root in candidates {
            let intervals = get_intervals(&normalized, root);

            if let Some(chord_type) = match_chord_type(intervals) {
                let mut name = format!("{}{}", note_name(root), chord_type);
                if bass != root {
                    name.push('/');
                    name.push_str(note_name(bass));
                }
                return Some(name);
            }
        }

        None
    }

    // TODO: This is clunky, we should change names based on the scale in use
    pub fn note_name(midi: u8) -> &'static str {
        const NAMES: [&str; 12] = [
            "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B",
        ];
        NAMES[(midi % 12) as usize]
    }

    fn get_intervals(notes: &[u8], root: u8) -> Vec<u8> {
        notes
            .iter()
            .map(|&n| (n + 12 - root) % 12)
            .filter(|&i| i != 0)
            .collect()
    }

    fn match_chord_type(mut intervals: Vec<u8>) -> Option<&'static str> {
        intervals.sort_unstable();

        match intervals.as_slice() {
            // Triads
            [3, 7] => Some("m"),   // minor
            [4, 7] => Some(""),    // major
            [3, 6] => Some("dim"), // diminished
            [4, 8] => Some("aug"), // augmented

            // Seventh chords
            [3, 7, 10] => Some("m7"),      // minor 7th
            [4, 7, 11] => Some("maj7"),    // major 7th
            [4, 7, 10] => Some("7"),       // dominant 7th
            [3, 6, 9] => Some("dim7"),     // diminished 7th
            [3, 6, 10] => Some("m7b5"),    // half-diminished
            [4, 8, 10] => Some("aug7"),    // augmented 7th
            [4, 8, 11] => Some("augmaj7"), // augmented major 7th
            [3, 7, 11] => Some("mmaj7"),   // minor major 7th

            // Sixth chords
            [4, 7, 9] => Some("6"),  // major 6th
            [3, 7, 9] => Some("m6"), // minor 6th

            // Suspended chords
            [2, 7] => Some("sus2"),      // suspended 2nd
            [5, 7] => Some("sus4"),      // suspended 4th
            [5, 7, 10] => Some("7sus4"), // dominant 7th suspended 4th

            // Extended chords (9ths)
            [2, 4, 7, 10] => Some("9"),    // dominant 9th
            [2, 4, 7, 11] => Some("maj9"), // major 9th
            [2, 3, 7, 10] => Some("m9"),   // minor 9th

            // Power chord
            [7] => Some("5"), // power chord

            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_major_chords() {
            assert_eq!(deduce_name(&[60, 64, 67]).unwrap(), "C"); // C major
            assert_eq!(deduce_name(&[62, 66, 69]).unwrap(), "D"); // D major
        }

        #[test]
        fn test_minor_chords() {
            assert_eq!(deduce_name(&[60, 63, 67]).unwrap(), "Cm"); // C minor
            assert_eq!(deduce_name(&[57, 60, 64]).unwrap(), "Am"); // A minor
        }

        #[test]
        fn test_seventh_chords() {
            assert_eq!(deduce_name(&[60, 64, 67, 71]).unwrap(), "Cmaj7"); // C major 7
            assert_eq!(deduce_name(&[60, 64, 67, 70]).unwrap(), "C7"); // C dominant 7
            assert_eq!(deduce_name(&[60, 63, 67, 70]).unwrap(), "Cm7"); // C minor 7
        }

        #[test]
        fn test_other_chords() {
            assert_eq!(deduce_name(&[60, 63, 66]).unwrap(), "Cdim"); // C diminished
            assert_eq!(deduce_name(&[60, 64, 68]).unwrap(), "Caug"); // C augmented
            assert_eq!(deduce_name(&[60, 65, 67]).unwrap(), "Csus4"); // C sus4
            assert_eq!(deduce_name(&[60, 67]).unwrap(), "C5"); // C power chord
        }

        #[test]
        fn test_slash_chords() {
            assert_eq!(deduce_name(&[64, 67, 72]).unwrap(), "C/E"); // C major, first inversion
            assert_eq!(deduce_name(&[67, 72, 76]).unwrap(), "C/G"); // C major, second inversion
            assert_eq!(deduce_name(&[57, 60, 64]).unwrap(), "Am");
        }

        #[test]
        fn test_edge_cases() {
            assert_eq!(deduce_name(&[]), None);
            assert_eq!(deduce_name(&[60]).unwrap(), "C");
            // Multiple octaves should normalize
            assert_eq!(deduce_name(&[48, 64, 67, 72]).unwrap(), "C");
        }
    }
}
