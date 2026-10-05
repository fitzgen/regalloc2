//! Capturing the input to a regicide allocation, to replay it elsewhere.
//!
//! Debugging a regicide allocation inside a large program means iterating on
//! one function, and rebuilding that program for every attempt is far slower
//! than replaying the allocation on its own. This module writes the exact
//! `regicide::Input` an allocation ran on, in `postcard`'s binary encoding,
//! for `regicide::deserialize` to read back:
//!
//! ```text
//! cargo run --example visualize --all-features -- --input FILE out.html
//! ```
//!
//! Binary rather than JSON because a capture is written for every function a
//! program compiles, and a whole corpus of them is what gets replayed:
//! `postcard` is both smaller on disk and quicker to write than JSON, and
//! regicide's examples detect either encoding, so an older JSON corpus still
//! replays.
//!
//! Enabled by the `regicide-dump` cargo feature, and then only when
//! `REGICIDE_DUMP` names a directory to write to. Unset, nothing is captured
//! and the filesystem is never touched.
//!
//! Every allocation is captured, whatever becomes of it. Failures are worth
//! replaying, but so is an allocation that succeeds and returns something
//! suspect, and the capture is written *before* the allocator runs, so an
//! allocation that aborts, panics or loops forever leaves its input behind
//! too.
//!
//! With `REGICIDE_DUMP_ONLY` also set, regicide is never run: the function is
//! allocated with fastalloc instead, which is far quicker and lets the client
//! carry on to the next function. That is for building a corpus, where only
//! the captures matter and compiling a whole program with regicide would
//! dominate the time taken.
//!
//! Failing to capture never fails an allocation: a directory that cannot be
//! created or a file that cannot be written is logged and otherwise ignored.
//!
//! # Naming
//!
//! A dump is named `<pid>-<counter>[-<function>].postcard`, where the counter
//! runs from zero within one process and the function's name is absent if it
//! has none. Both the process id and the counter are load-bearing: a test
//! suite runs many processes at once, each with its own counter starting at
//! zero, and each allocates many functions.

use crate::Function;
use alloc::{
    format,
    string::{String, ToString},
};
use core::sync::atomic::{AtomicU64, Ordering};
use std::{
    env,
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
    process,
    sync::OnceLock,
};

use ::regicide as rgc;

/// The longest the `<function>` component of a dump's name may be.
const MAX_NAME_COMPONENT: usize = 48;

/// The directory `REGICIDE_DUMP` named, once per process.
///
/// Read once rather than per allocation both because the allocator runs often
/// and because the whole facility is meant to behave the same for every
/// allocation in a run.
fn dump_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = PathBuf::from(env::var_os("REGICIDE_DUMP")?);
        if let Err(e) = fs::create_dir_all(&dir) {
            log::error!("REGICIDE_DUMP {} is unusable: {e}", dir.display());
            return None;
        }
        Some(dir)
    })
    .as_deref()
}

/// Whether `REGICIDE_DUMP_ONLY` asked to capture inputs without running
/// regicide on them.
///
/// Only when a capture is actually being written: an allocation that is
/// neither captured nor run by regicide is of no use to anyone.
pub(crate) fn dump_only() -> bool {
    static DUMP_ONLY: OnceLock<bool> = OnceLock::new();
    dump_dir().is_some() && *DUMP_ONLY.get_or_init(|| env::var_os("REGICIDE_DUMP_ONLY").is_some())
}

/// Capture `input` if `REGICIDE_DUMP` asked for it.
///
/// Called before the allocation rather than after it, so that an allocation
/// which never returns is captured as well.
///
/// `func` is the function being allocated, consulted only for
/// `Function::name`.
pub(crate) fn maybe_dump<F: Function>(input: &rgc::Input<'_, impl rgc::InputFunction>, func: &F) {
    /// Distinguishes the dumps one process writes.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let Some(dir) = dump_dir() else { return };

    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(file_name(counter, func.name().and_then(slug).as_deref()));
    match write(input, &path) {
        Ok(()) => log::info!("captured a regicide input in {}", path.display()),
        Err(e) => log::error!("could not write the regicide dump {}: {e}", path.display()),
    }
}

/// Serialize `input` to `path`, in `postcard`'s binary encoding.
fn write(input: &rgc::Input<'_, impl rgc::InputFunction>, path: &Path) -> Result<(), String> {
    let file = File::create(path).map_err(|e| e.to_string())?;
    postcard::to_io(
        &rgc::serialize::SerializeInput::new(input),
        BufWriter::new(file),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Build the name to write this dump under, from a `func` already reduced by
/// `slug`.
///
/// See the module docs for the shape and for why it is this shape.
fn file_name(counter: u64, func: Option<&str>) -> String {
    let mut name = format!("{}-{:04}", process::id(), counter);
    // The function is absent when there is none, rather than present and
    // empty, so that the name has no run of separators to read past.
    if let Some(func) = func {
        name.push('-');
        name.push_str(func);
    }
    name.push_str(".postcard");
    name
}

/// Reduce `text` to lowercase words joined by dashes, short enough to sit in a
/// file name.
///
/// Returns `None` only when `text` holds nothing usable. Every byte outside
/// `[0-9a-z]` is a separator, which keeps the name portable whatever
/// punctuation the client's names carry.
fn slug(text: &str) -> Option<String> {
    let mut slug = String::new();
    for word in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        // Prefer to end on a whole word, but a first word that is itself too
        // long is cut rather than dropped: a name is more use than none.
        if slug.len() + 1 + word.len() > MAX_NAME_COMPONENT {
            if slug.is_empty() {
                slug.push_str(&word[..MAX_NAME_COMPONENT].to_ascii_lowercase());
            }
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_ascii_lowercase());
    }

    (!slug.is_empty()).then_some(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("u3:0").unwrap(), "u3-0");
        assert_eq!(slug("some_helper").unwrap(), "some-helper");
        assert!(slug("").is_none());
        assert!(slug(":::").is_none());
    }

    /// A slug too long for a name ends on a whole word.
    #[test]
    fn slug_ends_on_a_word() {
        let slug = slug("a function with a rather long and quite unwieldy name").unwrap();
        assert_eq!(slug, "a-function-with-a-rather-long-and-quite-unwieldy");
        assert!(slug.len() <= MAX_NAME_COMPONENT);
    }

    /// A single word longer than the limit is cut, not dropped.
    #[test]
    fn slug_of_one_long_word() {
        let long = "x".repeat(MAX_NAME_COMPONENT * 2);
        assert_eq!(slug(&long).unwrap().len(), MAX_NAME_COMPONENT);
    }

    #[test]
    fn names() {
        let pid = process::id();
        assert_eq!(file_name(7, Some("u3-0")), format!("{pid}-0007-u3-0.postcard"));
        assert_eq!(file_name(7, None), format!("{pid}-0007.postcard"));
    }
}
