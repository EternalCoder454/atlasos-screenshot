//! Property tests of everything that reads untrusted input. The invariants
//! are in `checks.rs`, which the libFuzzer targets in `fuzz/` use too. The
//! runs are bounded (256 cases, 64 for the ones that write files) and leave
//! no failure file in the repository.

use proptest::collection::vec;
use proptest::prelude::*;

use crate::checks;

fn config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

/// Bytes with a few of `base`'s edits: a byte changed, one inserted, a run
/// cut out, a piece of `bits` inserted, or the end cut off.
fn mutated(base: Vec<u8>, ops: Vec<(u8, usize, u8)>, bits: &'static [&'static [u8]]) -> Vec<u8> {
    let mut v = base;
    for (op, at, b) in ops {
        let at = if v.is_empty() { 0 } else { at % (v.len() + 1) };
        match op % 5 {
            0 if at < v.len() => v[at] = b,
            1 => v.insert(at, b),
            2 if at < v.len() => {
                let end = (at + 1 + b as usize % 8).min(v.len());
                v.drain(at..end);
            }
            3 if !bits.is_empty() => {
                let piece = bits[b as usize % bits.len()];
                v.splice(at..at, piece.iter().copied());
            }
            4 => v.truncate(at),
            _ => {}
        }
    }
    v
}

fn edits() -> impl Strategy<Value = Vec<(u8, usize, u8)>> {
    vec((any::<u8>(), any::<usize>(), any::<u8>()), 1..6)
}

fn small_bytes(max: usize) -> impl Strategy<Value = Vec<u8>> {
    vec(any::<u8>(), 0..max)
}

// --- File names -----------------------------------------------------------

fn any_time() -> impl Strategy<Value = std::time::SystemTime> {
    use std::time::{Duration, UNIX_EPOCH};
    let secs = prop_oneof![
        0u64..4_000_000_000,
        250_000_000_000u64..260_000_000_000,
        any::<u64>(),
    ];
    (secs, any::<bool>(), 0u32..1_000_000_000).prop_filter_map("in range", |(s, before, ns)| {
        let d = Duration::new(s, ns);
        if before {
            UNIX_EPOCH.checked_sub(d)
        } else {
            UNIX_EPOCH.checked_add(d)
        }
    })
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn stamps_and_names_keep_their_shape(t in any_time()) {
        checks::stamp_time(t);
    }

    #[test]
    fn names_for_any_stamp_from_bytes(bytes in small_bytes(9)) {
        let mut data = bytes;
        data.resize(9, 0);
        checks::stamp_names(&data);
    }
}

// --- Saving ---------------------------------------------------------------

proptest! {
    #![proptest_config(config(64))]

    #[test]
    fn a_save_gives_back_the_bytes_at_0600(png in small_bytes(4096)) {
        checks::save_round_trip(&png);
    }

    #[test]
    fn a_taken_name_is_never_written_through_or_replaced(
        png in small_bytes(512),
        kinds in vec(any::<u8>(), 1..8),
        renamed in any::<bool>(),
    ) {
        checks::save_beside(&png, &kinds, renamed);
    }

    #[test]
    fn save_png_skips_planted_names(png in small_bytes(512), kinds in vec(any::<u8>(), 1..5)) {
        checks::save_png_beside(&png, &kinds);
    }
}

#[test]
fn with_every_name_taken_nothing_is_saved() {
    checks::save_all_taken(b"\x89PNG data");
}

#[test]
fn the_first_name_as_a_dangling_link_outside_the_folder() {
    // The case the planted-link properties are for, spelled out.
    for renamed in [false, true] {
        checks::save_beside(b"x", &[2], renamed);
        checks::save_beside(b"x", &[1], renamed);
        checks::save_beside(b"x", &[0, 1, 2, 3], renamed);
    }
}

// --- user-dirs.dirs -------------------------------------------------------

fn user_dirs_line() -> impl Strategy<Value = String> {
    let value = prop_oneof![
        "\\$HOME(/[A-Za-z0-9 ._-]{0,8}){0,3}",
        "(/[A-Za-z0-9._-]{0,8}){0,4}",
        "\\$HOME/(\\.\\./){0,2}[a-z]{0,4}",
        "[\\x00-\\x7f]{0,24}",
        "\\PC{0,24}",
    ];
    let name = prop_oneof![Just("PICTURES".to_string()), "[A-Z]{1,8}"];
    (name, value, 0u8..4).prop_map(|(n, v, q)| match q {
        0 => format!("XDG_{n}_DIR=\"{v}\""),
        1 => format!("XDG_{n}_DIR={v}"),
        2 => format!("# XDG_{n}_DIR=\"{v}\""),
        _ => format!("  XDG_{n}_DIR=\"{v}\"  "),
    })
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn user_dirs_from_noise(text in "\\PC{0,200}", name in "[A-Z]{0,8}") {
        checks::user_dirs_text(&text, &name);
    }

    #[test]
    fn user_dirs_from_lines(lines in vec(user_dirs_line(), 0..5)) {
        let text = lines.join("\n");
        checks::user_dirs_text(&text, "PICTURES");
    }

    #[test]
    fn user_dirs_from_raw_bytes(data in small_bytes(200)) {
        checks::user_dirs(&data);
    }
}

// --- file:// links --------------------------------------------------------

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn links_are_plain_and_decode_to_the_path(path in small_bytes(120)) {
        checks::uri_bytes(&path);
    }

    #[test]
    fn links_from_text_paths(path in "[ -~\\u{80}-\\u{2fff}]{0,60}") {
        checks::uri_bytes(path.as_bytes());
    }
}

// --- The helper's job -----------------------------------------------------

fn valid_job() -> impl Strategy<Value = Vec<u8>> {
    (
        "\\PC{0,40}",
        "\\PC{0,80}",
        proptest::option::of(small_bytes(300)),
        proptest::option::of(small_bytes(60)),
        proptest::option::of((any::<u32>(), any::<u32>(), small_bytes(32))),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(title, body, png, path, thumb, edit, notify)| {
            // A job as the program writes it: build it the way the check does.
            let path = path.map(|p| {
                let mut v = b"/".to_vec();
                v.extend(p);
                std::path::PathBuf::from(
                    <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&v),
                )
            });
            let image = thumb.map(|(w, h, seed)| {
                let (w, h) = (1 + w % 8, 1 + h % 8);
                let rgba = (0..w as usize * h as usize * 4)
                    .map(|i| seed.get(i % seed.len().max(1)).copied().unwrap_or(0))
                    .collect();
                crate::notify::Image::Thumb { w, h, rgba }
            });
            crate::post::encode(&crate::post::Done {
                title,
                body,
                png,
                path,
                image,
                edit,
                notify,
            })
        })
}

const JOB_PIECES: &[&[u8]] = &[
    b"png:99999999999999999999\n",
    b"png:18446744073709551615\n",
    b"title:-1\n",
    b"thumb:8\n\x00\x00\x00\x00\x00\x00\x00\x00",
    b"thumb:12\n\xff\xff\xff\xff\xff\xff\xff\xff\x00\x00\x00\x00",
    b"path:3\nabc",
    b"\n",
    b":",
];

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn jobs_round_trip(
        title in "\\PC{0,60}",
        body in "\\PC{0,120}",
        png in proptest::option::of(small_bytes(400)),
        path in proptest::option::of(small_bytes(80)),
        thumb in proptest::option::of((any::<u32>(), any::<u32>(), small_bytes(40))),
        edit in any::<bool>(),
        notify in any::<bool>(),
    ) {
        checks::job_round_trip(
            &title,
            &body,
            png.as_deref(),
            path.as_deref(),
            thumb.as_ref().map(|(w, h, s)| (*w, *h, s.as_slice())),
            edit,
            notify,
        );
    }

    #[test]
    fn decode_of_noise(data in small_bytes(400)) {
        checks::post_decode(&data);
    }

    #[test]
    fn decode_of_text_that_looks_like_a_job(
        parts in vec(("(title|body|png|path|thumb|edit|notify|x)", "[0-9a-z+-]{0,22}", small_bytes(12)), 0..6),
    ) {
        let mut data = Vec::new();
        for (name, len, body) in parts {
            data.extend_from_slice(format!("{name}:{len}\n").as_bytes());
            data.extend_from_slice(&body);
        }
        checks::post_decode(&data);
    }

    #[test]
    fn decode_of_cut_jobs(job in valid_job(), at in any::<usize>()) {
        let at = at % (job.len() + 1);
        checks::post_decode(&job[..at]);
        checks::post_decode(&job[at..]);
    }

    #[test]
    fn decode_of_changed_jobs(job in valid_job(), ops in edits()) {
        checks::post_decode(&mutated(job, ops, JOB_PIECES));
    }
}

// --- The command line -----------------------------------------------------

const FLAGS: &[&str] = &[
    "-h",
    "-V",
    "--help",
    "--version",
    "--save-png",
    "--copy-png",
    "--dbus",
    "--post",
    "-f",
    "--full",
    "-m",
    "--screen",
    "-a",
    "--active-window",
    "-u",
    "--window",
    "-r",
    "--region",
    "--delay",
    "--mode",
    "--no-save",
    "--no-notify",
    "--notify",
    "--requested-by",
    "--edit",
    "--cursor",
    "--no-cursor",
    "--frame",
    "--no-frame",
    "--shadow",
    "--no-shadow",
];

fn arg() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => proptest::sample::select(FLAGS).prop_map(str::to_string),
        1 => "\\PC{0,12}",
        1 => "-{0,2}[a-z-]{0,10}",
        2 => "-?[0-9]{1,7}",
        2 => "-?[0-9]{1,6} ?, ?-?[0-9]{1,6} ?, ?[0-9]{1,6} ?x ?[0-9]{1,6}",
        1 => proptest::sample::select(&["image", "text", "redact", "TEXT", ""][..]).prop_map(str::to_string),
        1 => "[ -~]{60,80}",
    ]
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn arguments_never_panic_and_stay_in_bounds(args in vec(arg(), 0..10)) {
        checks::args_list(&args);
    }

    #[test]
    fn arguments_of_flags_with_values(
        kind in proptest::sample::select(&["-f", "-m", "-a", "-u", "-r", "--region", "--window"][..]),
        delay in "[0-9]{0,4}",
        who in "\\PC{0,70}",
        region in "-?[0-9]{1,6},-?[0-9]{1,6},[0-9]{1,6}x[0-9]{1,6}",
        notify in any::<bool>(),
    ) {
        let mut args = vec![kind.to_string(), "--delay".into(), delay, "--requested-by".into(), who];
        args.push(if notify { "--notify".into() } else { "--no-notify".into() });
        checks::args_list(&args);
        let mut with_region = vec!["--region".to_string(), region];
        with_region.extend(args);
        checks::args_list(&with_region);
    }

    #[test]
    fn arguments_from_bytes(data in small_bytes(200)) {
        checks::cli_args(&data);
    }

    #[test]
    fn regions_from_noise(s in "\\PC{0,30}") {
        checks::region_text(&s);
    }

    #[test]
    fn regions_from_numbers(s in "[ +-]{0,2}[0-9]{0,7}[ ,]{0,2}[ +-]{0,2}[0-9]{0,7}[ ,]{0,2}[0-9]{0,7}[x ]{0,2}[0-9]{0,7}") {
        checks::region_text(&s);
    }
}

// --- The config file ------------------------------------------------------

const EXAMPLE: &str = include_str!("../data/config.example.toml");

const CONFIG_PIECES: &[&[u8]] = &[
    b"dim = 0.9\n",
    b"dim = nan\n",
    b"dim = inf\n",
    b"dim = -0.0\n",
    b"padding = 4294967295\n",
    b"padding = -1\n",
    b"accent = \"#GGGGGG\"\n",
    b"fill = \"#12345\"\n",
    b"save_dir = \"relative/dir\"\n",
    b"save_dir = \"/a/../b\"\n",
    b"default_mode = \"nothing\"\n",
    b"[output]\n",
    b"[[capture]]\n",
    b"[",
    b"\"\"\"",
    b"unknown = 1\n",
    b"\xff",
];

fn config_text() -> impl Strategy<Value = String> {
    prop_oneof![
        "\\PC{0,200}",
        "([a-z_.]{1,12} = (true|false|[0-9.eE+-]{1,10}|\"[#0-9A-Za-z/._-]{0,12}\")\n|\\[[a-z.]{0,10}\\]\n){0,8}",
        // The example, with edits, and with its values swapped for odd ones.
        (edits()).prop_map(|ops| {
            String::from_utf8_lossy(&mutated(EXAMPLE.as_bytes().to_vec(), ops, CONFIG_PIECES))
                .into_owned()
        }),
    ]
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn config_parse_never_panics_and_validates(text in config_text()) {
        checks::config_text(&text);
    }

    #[test]
    fn config_parse_of_bytes(data in small_bytes(300)) {
        checks::config_parse(&data);
    }
}

#[test]
fn the_example_config_is_a_valid_one() {
    let c = crate::config::parse(EXAMPLE).expect("the example parses");
    checks::config_ok(&c);
    assert_eq!(c, crate::config::Config::default());
}

// --- D-Bus callers and notification text -----------------------------------

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn callers_are_named_plainly(comm in small_bytes(40), exe in proptest::option::of(small_bytes(40))) {
        checks::comm_bytes(&comm, exe.as_deref());
    }

    #[test]
    fn callers_from_the_bytes_a_program_may_choose(comm in vec(prop_oneof![
        Just(b'\n'), Just(0u8), Just(b' '), Just(b'<'), Just(b'-'), Just(0xffu8), 0x20u8..0x7f,
    ], 0..24), exe in proptest::option::of(vec(prop_oneof![
        Just(b'\n'), Just(0u8), Just(b' '), Just(b'('), Just(b')'), 0x20u8..0x7f,
    ], 0..40))) {
        checks::comm_bytes(&comm, exe.as_deref());
    }

    #[test]
    fn command_lines_for_any_numbers_parse(
        what in any::<u8>(),
        p in any::<i32>(),
        f in any::<i32>(),
        s in any::<i32>(),
        comm in small_bytes(20),
        exe in proptest::option::of(small_bytes(30)),
    ) {
        let label = checks::label_of(&comm, exe.as_deref());
        checks::argv_parses(what, p, f, s, &label);
    }

    #[test]
    fn command_lines_for_any_label_parse_or_are_refused(
        what in any::<u8>(),
        label in "\\PC{0,80}",
    ) {
        checks::argv_parses(what, -1, -1, -1, &label);
    }

    #[test]
    fn escaped_text_is_not_markup(s in "[<>&a-z;\\u{0}-\\u{7f}]{0,40}") {
        checks::escape_text(&s);
    }

    #[test]
    fn escaped_any_text(s in "\\PC{0,60}") {
        checks::escape_text(&s);
    }
}

// --- The PNG on stdin ------------------------------------------------------

const PNG_PIECES: &[&[u8]] = &[
    b"\x00\x00\x40\x01",
    b"\x00\x00\x40\x00",
    b"\x00\x00\x00\x00",
    b"\xff\xff\xff\xff",
    b"IHDR",
    b"IDAT",
    b"\x89PNG\r\n\x1a\n",
];

fn small_png() -> impl Strategy<Value = Vec<u8>> {
    (1u32..8, 1u32..8, small_bytes(16)).prop_map(|(w, h, seed)| {
        use image::ImageEncoder;
        let raw: Vec<u8> = (0..w as usize * h as usize * 4)
            .map(|i| seed.get(i % seed.len().max(1)).copied().unwrap_or(7))
            .collect();
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&raw, w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        png
    })
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn pictures_from_noise_never_panic(data in small_bytes(200)) {
        checks::png_header(&data);
    }

    #[test]
    fn pictures_from_a_header_and_noise(tail in small_bytes(100), cut in 0usize..40) {
        let mut data = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        data.extend(tail);
        data.truncate(data.len().max(20).min(20 + cut));
        checks::png_header(&data);
    }

    #[test]
    fn real_pictures_are_taken_unchanged(w in any::<u32>(), h in any::<u32>(), seed in small_bytes(64)) {
        checks::png_round_trip(w, h, &seed);
    }

    #[test]
    fn real_pictures_with_changes(png in small_png(), ops in edits()) {
        checks::png_header(&mutated(png, ops, PNG_PIECES));
    }

    #[test]
    fn pictures_with_a_size_that_is_made_up(png in small_png(), w in any::<u32>(), h in any::<u32>()) {
        let mut png = png;
        png[16..20].copy_from_slice(&w.to_be_bytes());
        png[20..24].copy_from_slice(&h.to_be_bytes());
        checks::png_header(&png);
        let sized = (1..=16384).contains(&w) && (1..=16384).contains(&h);
        checks::fix_ihdr_crc(&mut png);
        // A made-up size is judged by the size rule, whatever else is wrong.
        if !sized {
            assert!(crate::pngin::read_png(&png[..]).is_err());
        }
    }
}

// --- Redaction ---------------------------------------------------------------

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn redaction_ranges_are_in_bounds(text in "\\PC{0,200}", flags in any::<u8>()) {
        checks::redact_flags(&text, flags);
    }

    #[test]
    fn redaction_of_things_that_look_like_addresses(
        text in "([0-9A-Fa-fOlI|]{1,3}[.:@ ,-]{0,2}|\\u{e9}|[a-z]{1,4}@[a-z]{1,4}\\.[a-z]{2}|::|%en0 ){0,16}",
        flags in 0u8..16,
    ) {
        checks::redact_flags(&text, flags);
    }

    #[test]
    fn redaction_of_bytes(data in small_bytes(200)) {
        checks::redact_text(&data);
    }
}
