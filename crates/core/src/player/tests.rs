use super::output::SilentOutput;
use super::*;
use crate::engine::Voice;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One second of audio per sentence at rate 1.0, shorter when faster.
struct FakeEngine {
    calls: AtomicUsize,
    log: Mutex<Vec<SynthRequest>>,
    fail_on: Option<&'static str>,
    /// How long one synthesis takes (a neural engine is not instant).
    delay: Duration,
}

impl FakeEngine {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
            fail_on: None,
            delay: Duration::ZERO,
        })
    }
}

impl Engine for FakeEngine {
    fn id(&self) -> &str {
        "fake"
    }
    fn name(&self) -> &str {
        "Fake"
    }
    fn voices(&self) -> anyhow::Result<Vec<Voice>> {
        Ok(vec![])
    }
    fn synthesize(&self, req: &SynthRequest) -> anyhow::Result<Audio> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log.lock().unwrap().push(req.clone());
        std::thread::sleep(self.delay);
        if self.fail_on.is_some_and(|f| req.text.contains(f)) {
            anyhow::bail!("engine exploded");
        }
        let n = (1000.0 / req.rate) as usize;
        Ok(Audio {
            sample_rate: 1000,
            samples: vec![0.5; n],
        })
    }
}

fn utterances(n: usize) -> Vec<Utterance> {
    (0..n)
        .map(|i| Utterance {
            role: if i == 0 {
                Role::Heading
            } else {
                Role::Paragraph
            },
            spoken: format!("sentence {i}"),
            display: format!("Sentence {i}"),
            ends_block: i == 0 || i + 1 == n,
        })
        .collect()
}

fn config(engine: Arc<FakeEngine>) -> PlayerConfig {
    let mut roles = RoleStyles::default();
    for role in Role::ALL {
        roles.get_mut(role).pause_after_ms = 0;
    }
    PlayerConfig {
        engine: Some(engine),
        voice: Some("main".into()),
        volume: 1.0,
        sentence_pause_ms: 0,
        roles,
    }
}

/// Player whose clock runs `speed` times faster than real time.
fn player(engine: Arc<FakeEngine>, speed: f64) -> Player {
    Player::new(
        move || Box::new(SilentOutput::with_speed(speed)),
        config(engine),
        1.0,
        || {},
    )
}

fn wait_for(player: &Player, what: &str, pred: impl Fn(&PlayerSnapshot) -> bool) -> PlayerSnapshot {
    let start = Instant::now();
    loop {
        let snap = player.snapshot();
        if pred(&snap) {
            return snap;
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!("timed out waiting for {what}; last snapshot: {snap:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn plays_through_and_returns_to_idle() {
    let engine = FakeEngine::new();
    let p = player(engine.clone(), 20.0);
    p.speak(utterances(3), "Test");
    let snap = wait_for(&p, "playing", |s| s.status == Status::Playing);
    assert_eq!(snap.total, 3);
    assert_eq!(snap.title, "Test");
    assert_eq!(snap.current.as_deref(), Some("Sentence 0"));
    wait_for(&p, "second sentence", |s| s.index == 1);
    wait_for(&p, "third sentence", |s| s.index == 2);
    let snap = wait_for(&p, "idle", |s| s.status == Status::Idle);
    // Text stays loaded, rewound to the start.
    assert_eq!((snap.index, snap.total), (0, 3));
    assert_eq!(engine.calls.load(Ordering::SeqCst), 3);

    // Play again from idle restarts from the top.
    p.toggle_pause();
    wait_for(&p, "replay", |s| {
        s.status == Status::Playing && s.index == 0
    });
}

#[test]
fn pause_holds_position_and_resume_continues() {
    let p = player(FakeEngine::new(), 1.0);
    p.speak(utterances(3), "");
    wait_for(&p, "playing", |s| s.status == Status::Playing);
    p.toggle_pause();
    wait_for(&p, "paused", |s| s.status == Status::Paused);
    std::thread::sleep(Duration::from_millis(1200));
    // More than a sentence's worth of time passed, but we did not advance.
    assert_eq!(p.snapshot().index, 0);
    p.toggle_pause();
    wait_for(&p, "playing again", |s| s.status == Status::Playing);
    wait_for(&p, "advance after resume", |s| s.index == 1);
}

#[test]
fn next_previous_and_stop() {
    let p = player(FakeEngine::new(), 0.2);
    p.speak(utterances(4), "");
    wait_for(&p, "playing", |s| s.status == Status::Playing);
    p.next();
    wait_for(&p, "index 1", |s| {
        s.index == 1 && s.status == Status::Playing
    });
    p.next();
    wait_for(&p, "index 2", |s| {
        s.index == 2 && s.status == Status::Playing
    });
    // Just started this sentence, so previous goes back one.
    p.previous();
    wait_for(&p, "back to 1", |s| {
        s.index == 1 && s.status == Status::Playing
    });
    p.previous();
    p.previous();
    p.previous();
    wait_for(&p, "clamped at 0", |s| {
        s.index == 0 && s.status == Status::Playing
    });
    p.stop();
    let snap = wait_for(&p, "stopped", |s| s.status == Status::Idle);
    assert_eq!(snap.index, 0);
    assert!(snap.error.is_none());
}

#[test]
fn previous_restarts_sentence_when_well_into_it() {
    // 4x clock and half speed: each sentence is 2 s of audio, 500 ms real.
    let p = player(FakeEngine::new(), 4.0);
    p.set_rate(0.5);
    p.speak(utterances(3), "");
    wait_for(&p, "second sentence", |s| {
        s.index == 1 && s.status == Status::Playing
    });
    // 420 ms real = 1.68 s into the sentence: past the restart threshold.
    std::thread::sleep(Duration::from_millis(420));
    p.previous();
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(
        p.snapshot().index,
        1,
        "should restart sentence 1, not go to 0"
    );
}

#[test]
fn next_on_last_sentence_finishes() {
    let p = player(FakeEngine::new(), 0.2);
    p.speak(utterances(1), "");
    wait_for(&p, "playing", |s| s.status == Status::Playing);
    p.next();
    wait_for(&p, "idle", |s| s.status == Status::Idle);
}

#[test]
fn rate_changes_apply_immediately_and_clamp() {
    let engine = FakeEngine::new();
    let p = player(engine.clone(), 0.2);
    p.speak(utterances(3), "");
    wait_for(&p, "playing", |s| s.status == Status::Playing);
    p.faster();
    p.faster();
    let snap = wait_for(&p, "rate 1.5", |s| {
        (s.rate - 1.5).abs() < 1e-4 && s.status == Status::Playing
    });
    assert_eq!(snap.index, 0, "speed change must not skip sentences");
    assert!(engine
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.text == "sentence 0" && (r.rate - 1.5).abs() < 1e-4));
    for _ in 0..100 {
        p.slower();
    }
    wait_for(&p, "min rate", |s| s.rate == crate::settings::MIN_RATE);
    p.set_rate(100.0);
    wait_for(&p, "max rate", |s| s.rate == crate::settings::MAX_RATE);
}

#[test]
fn rate_can_change_while_idle() {
    let p = player(FakeEngine::new(), 1.0);
    p.faster();
    let snap = wait_for(&p, "rate 1.25", |s| (s.rate - 1.25).abs() < 1e-4);
    assert_eq!(snap.status, Status::Idle);
}

#[test]
fn skipping_while_paused_stays_paused() {
    let p = player(FakeEngine::new(), 0.2);
    p.speak(utterances(3), "");
    wait_for(&p, "playing", |s| s.status == Status::Playing);
    p.pause();
    p.next();
    let snap = wait_for(&p, "moved while paused", |s| s.index == 1);
    assert_eq!(snap.status, Status::Paused);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(p.snapshot().status, Status::Paused);
    p.toggle_pause();
    wait_for(&p, "playing", |s| {
        s.status == Status::Playing && s.index == 1
    });
}

#[test]
fn engine_failure_surfaces_as_error() {
    let engine = Arc::new(FakeEngine {
        calls: AtomicUsize::new(0),
        log: Mutex::new(Vec::new()),
        fail_on: Some("sentence 1"),
        delay: Duration::ZERO,
    });
    let p = player(engine, 20.0);
    p.speak(utterances(3), "");
    let snap = wait_for(&p, "error", |s| s.error.is_some());
    assert_eq!(snap.status, Status::Idle);
    assert!(snap.error.unwrap().contains("engine exploded"));
    // A new request clears the error.
    p.speak(utterances(1), "");
    wait_for(&p, "playing", |s| {
        s.status == Status::Playing && s.error.is_none()
    });
}

#[test]
fn empty_text_and_missing_engine_report_errors() {
    let p = player(FakeEngine::new(), 1.0);
    p.speak(Vec::new(), "");
    let snap = wait_for(&p, "error", |s| s.error.is_some());
    assert_eq!(snap.status, Status::Idle);

    let mut cfg = config(FakeEngine::new());
    cfg.engine = None;
    let p = Player::new(|| Box::new(SilentOutput::new()), cfg, 1.0, || {});
    p.speak(utterances(2), "");
    let snap = wait_for(&p, "engine error", |s| s.error.is_some());
    assert_eq!(snap.status, Status::Idle);
}

#[test]
fn role_styles_reach_the_engine() {
    let engine = FakeEngine::new();
    let mut cfg = config(engine.clone());
    cfg.roles.heading.voice = "narrator".into();
    cfg.roles.heading.rate = 0.5;
    cfg.roles.heading.announce = "Heading:".into();
    cfg.roles.paragraph.pitch = 1.5;
    let p = Player::new(|| Box::new(SilentOutput::with_speed(50.0)), cfg, 2.0, || {});
    p.speak(utterances(3), "");
    wait_for(&p, "finished", |s| s.status == Status::Idle && s.total == 3);
    let log = engine.log.lock().unwrap();
    let heading = log.iter().find(|r| r.text.contains("sentence 0")).unwrap();
    assert_eq!(heading.text, "Heading: sentence 0");
    assert_eq!(heading.voice.as_deref(), Some("narrator"));
    assert!((heading.rate - 1.0).abs() < 1e-4);
    let para = log.iter().find(|r| r.text == "sentence 1").unwrap();
    assert_eq!(para.voice.as_deref(), Some("main"));
    assert!((para.rate - 2.0).abs() < 1e-4);
    assert!((para.pitch - 1.5).abs() < 1e-4);
}

#[test]
fn reconfigure_mid_playback_uses_new_voice() {
    let engine = FakeEngine::new();
    let p = player(engine.clone(), 0.2);
    p.speak(utterances(2), "");
    wait_for(&p, "playing", |s| s.status == Status::Playing);
    let mut cfg = config(engine.clone());
    cfg.voice = Some("other".into());
    p.configure(cfg);
    let start = Instant::now();
    loop {
        if engine
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.voice.as_deref() == Some("other") && r.text == "sentence 0")
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "voice change not applied"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    wait_for(&p, "still playing sentence 0", |s| {
        s.status == Status::Playing && s.index == 0
    });
}

#[test]
fn notifies_on_change() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let p = Player::new(
        || Box::new(SilentOutput::with_speed(50.0)),
        config(FakeEngine::new()),
        1.0,
        move || {
            c.fetch_add(1, Ordering::SeqCst);
        },
    );
    p.speak(utterances(2), "");
    wait_for(&p, "done", |s| s.status == Status::Idle && s.total == 2);
    assert!(count.load(Ordering::SeqCst) >= 3);
}

#[test]
fn slow_engine_keeps_playing_while_new_speed_is_prepared() {
    let engine = Arc::new(FakeEngine {
        calls: AtomicUsize::new(0),
        log: Mutex::new(Vec::new()),
        fail_on: None,
        delay: Duration::from_millis(250),
    });
    // Real-time clock: a sentence is one second long.
    let p = player(engine.clone(), 1.0);
    p.speak(utterances(2), "");
    wait_for(&p, "playing", |s| s.status == Status::Playing);
    p.faster();
    // While the engine works on the faster version, the old audio goes on:
    // the player must never fall back to "loading".
    let start = Instant::now();
    let mut switched = false;
    while start.elapsed() < Duration::from_millis(600) {
        let snap = p.snapshot();
        assert_eq!(snap.status, Status::Playing, "gap while changing speed");
        assert_eq!(snap.index, 0);
        switched |= engine
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.text == "sentence 0" && (r.rate - 1.25).abs() < 1e-4);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(switched, "the faster version was never requested");
    // Skipping, in contrast, interrupts immediately.
    p.next();
    wait_for(&p, "loading next", |s| s.index == 1);
}

/// Counts how sentences reach the output: started afresh (`play`) or handed
/// over inside the running stream (`queue_next`).
struct CountingOutput {
    inner: SilentOutput,
    plays: Arc<AtomicUsize>,
    handovers: Arc<AtomicUsize>,
}

impl AudioOutput for CountingOutput {
    fn play(&mut self, audio: Arc<Audio>, start: usize) -> anyhow::Result<()> {
        self.plays.fetch_add(1, Ordering::SeqCst);
        self.inner.play(audio, start)
    }
    fn queue_next(&mut self, audio: Arc<Audio>) -> bool {
        self.inner.queue_next(audio)
    }
    fn clear_next(&mut self) -> bool {
        self.inner.clear_next()
    }
    fn advanced(&mut self) -> bool {
        let advanced = self.inner.advanced();
        if advanced {
            self.handovers.fetch_add(1, Ordering::SeqCst);
        }
        advanced
    }
    fn pause(&mut self) {
        self.inner.pause()
    }
    fn resume(&mut self) {
        self.inner.resume()
    }
    fn stop(&mut self) {
        self.inner.stop()
    }
    fn position(&self) -> usize {
        self.inner.position()
    }
    fn finished(&self) -> bool {
        self.inner.finished()
    }
}

fn counting_player(
    engine: Arc<FakeEngine>,
    speed: f64,
    rate: f32,
) -> (Player, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let plays = Arc::new(AtomicUsize::new(0));
    let handovers = Arc::new(AtomicUsize::new(0));
    let (p, h) = (plays.clone(), handovers.clone());
    let player = Player::new(
        move || {
            Box::new(CountingOutput {
                inner: SilentOutput::with_speed(speed),
                plays: p,
                handovers: h,
            })
        },
        config(engine),
        rate,
        || {},
    );
    (player, plays, handovers)
}

#[test]
fn sentences_hand_over_inside_the_stream() {
    let (p, plays, handovers) = counting_player(FakeEngine::new(), 8.0, 1.0);
    p.speak(utterances(5), "");
    let mut seen = Vec::new();
    let start = Instant::now();
    loop {
        let snap = p.snapshot();
        if snap.status == Status::Idle && !seen.is_empty() {
            break;
        }
        if snap.status == Status::Playing && seen.last() != Some(&snap.index) {
            seen.push(snap.index);
        }
        assert!(start.elapsed() < Duration::from_secs(5), "stuck: {snap:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        seen,
        vec![0, 1, 2, 3, 4],
        "every sentence is visited in order"
    );
    // Only the very first sentence starts a stream; the rest follow in it.
    assert_eq!(plays.load(Ordering::SeqCst), 1);
    assert_eq!(handovers.load(Ordering::SeqCst), 4);
}

#[test]
fn no_stalls_when_the_engine_is_slow_but_keeps_up() {
    // Each sentence takes 300 ms to synthesise and lasts 400 ms at 2.5x.
    let engine = Arc::new(FakeEngine {
        calls: AtomicUsize::new(0),
        log: Mutex::new(Vec::new()),
        fail_on: None,
        delay: Duration::from_millis(300),
    });
    let (p, _, _) = counting_player(engine, 1.0, 2.5);
    let n = 8;
    p.speak(utterances(n), "");
    wait_for(&p, "first sentence", |s| s.status == Status::Playing);
    let started = Instant::now();
    let mut waiting = Duration::ZERO;
    let mut last = Instant::now();
    loop {
        let snap = p.snapshot();
        let now = Instant::now();
        if snap.status == Status::Loading {
            waiting += now - last;
        }
        last = now;
        if snap.status == Status::Idle {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "stuck: {snap:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        waiting < Duration::from_millis(60),
        "reading stalled for {waiting:?} between sentences"
    );
    let audio = Duration::from_secs_f32(n as f32 / 2.5);
    assert!(
        started.elapsed() < audio + Duration::from_millis(250),
        "took {:?} for {audio:?} of audio",
        started.elapsed()
    );
}

#[test]
fn skipping_takes_back_the_lined_up_sentence() {
    let (p, plays, _) = counting_player(FakeEngine::new(), 0.2, 1.0);
    p.speak(utterances(5), "");
    wait_for(&p, "playing", |s| s.status == Status::Playing);
    // Give the player time to line up sentence 1 behind sentence 0.
    std::thread::sleep(Duration::from_millis(80));
    p.next();
    p.next();
    wait_for(&p, "sentence 2", |s| {
        s.index == 2 && s.status == Status::Playing
    });
    p.previous();
    wait_for(&p, "sentence 1", |s| {
        s.index == 1 && s.status == Status::Playing
    });
    // Each jump starts the stream afresh at the right sentence.
    assert_eq!(plays.load(Ordering::SeqCst), 4);
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(
        p.snapshot().index,
        1,
        "a stale lined-up sentence must not take over"
    );
}

#[test]
fn a_voice_with_another_sample_rate_starts_its_own_stream() {
    struct TwoRates;
    impl Engine for TwoRates {
        fn id(&self) -> &str {
            "two"
        }
        fn name(&self) -> &str {
            "Two"
        }
        fn voices(&self) -> anyhow::Result<Vec<Voice>> {
            Ok(vec![])
        }
        fn synthesize(&self, req: &SynthRequest) -> anyhow::Result<Audio> {
            // The heading voice has a different sample rate.
            let rate = if req.voice.as_deref() == Some("narrator") {
                2000
            } else {
                1000
            };
            Ok(Audio {
                sample_rate: rate,
                samples: vec![0.5; rate as usize],
            })
        }
    }
    let plays = Arc::new(AtomicUsize::new(0));
    let handovers = Arc::new(AtomicUsize::new(0));
    let (pc, hc) = (plays.clone(), handovers.clone());
    let mut cfg = config(FakeEngine::new());
    cfg.engine = Some(Arc::new(TwoRates));
    cfg.roles.heading.voice = "narrator".into();
    let p = Player::new(
        move || {
            Box::new(CountingOutput {
                inner: SilentOutput::with_speed(10.0),
                plays: pc,
                handovers: hc,
            })
        },
        cfg,
        1.0,
        || {},
    );
    // Sentence 0 is a heading (narrator), 1 and 2 are paragraphs.
    p.speak(utterances(3), "");
    wait_for(&p, "done", |s| {
        s.status == Status::Idle && s.total == 3 && s.index == 0
    });
    wait_for(&p, "all played", |_| plays.load(Ordering::SeqCst) == 2);
    assert_eq!(handovers.load(Ordering::SeqCst), 1);
}
