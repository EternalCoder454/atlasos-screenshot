#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_screenshot_fuzz::checks;

fuzz_target!(|data: &[u8]| {
    checks::redact_text(data);
});
