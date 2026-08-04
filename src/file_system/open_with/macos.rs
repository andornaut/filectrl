//! Application lookup via Launch Services.
//!
//! Every objc2 binding used here is generated as a safe `pub fn`, so the
//! crate-wide `unsafe_code = "deny"` lint needs no exception.

use std::path::Path;

use log::debug;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSFileManager, NSOperatingSystemVersion, NSProcessInfo, NSString, NSURL};

use super::AppCandidate;

/// Launching through `open` keeps this on the same detached spawn path as every
/// other platform, and avoids the asynchronous completion handler that
/// NSWorkspace's launch API requires.
const OPEN: &str = "/usr/bin/open";

/// `URLsForApplicationsToOpenURL:` was added in macOS 12. Sending it to an
/// older system raises an unrecognized selector, which aborts the process.
const MINIMUM_VERSION: NSOperatingSystemVersion = NSOperatingSystemVersion {
    majorVersion: 12,
    minorVersion: 0,
    patchVersion: 0,
};

pub(super) fn candidates_for(path: &Path) -> Vec<AppCandidate> {
    if !NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(MINIMUM_VERSION) {
        debug!("Launch Services lookup requires macOS 12 or newer");
        return Vec::new();
    }
    let workspace = NSWorkspace::sharedWorkspace();
    let url = NSURL::fileURLWithPath_isDirectory(
        &NSString::from_str(&path.to_string_lossy()),
        path.is_dir(),
    );
    let default = workspace
        .URLForApplicationToOpenURL(&url)
        .and_then(|bundle| bundle_path(&bundle));
    let mut candidates: Vec<AppCandidate> = workspace
        .URLsForApplicationsToOpenURL(&url)
        .to_vec()
        .iter()
        .filter_map(|bundle| bundle_path(bundle))
        .map(|bundle| {
            let is_default = default.as_deref() == Some(bundle.as_str());
            to_candidate(path, is_default, bundle)
        })
        .collect();
    // Launch Services documents no order for the returned array, so impose one
    // and hoist the default application to the top.
    candidates.sort_by_key(|candidate| (!candidate.is_default, candidate.name.to_lowercase()));
    candidates
}

fn bundle_path(url: &NSURL) -> Option<String> {
    url.path().map(|path| path.to_string())
}

fn to_candidate(path: &Path, is_default: bool, bundle: String) -> AppCandidate {
    let name = NSFileManager::defaultManager()
        .displayNameAtPath(&NSString::from_str(&bundle))
        .to_string();
    AppCandidate {
        argv: vec![
            OPEN.into(),
            "-a".into(),
            bundle.clone().into(),
            // The path's own bytes, like every other platform's argv: a lossy
            // conversion would hand `open` replacement characters and it would
            // open nothing.
            //
            // No "--" terminator: `open` documents "--args", not "--", so a
            // "--" could be taken as a filename operand. `candidates_for`
            // guarantees an absolute path, which can never look like a flag.
            path.as_os_str().to_os_string(),
        ],
        // The bundle path is what tells two identically named applications
        // apart, which a display name cannot.
        detail: bundle,
        is_default,
        name,
        working_dir: None,
    }
}
