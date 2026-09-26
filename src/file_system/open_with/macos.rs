//! Application lookup via Launch Services. Every objc2 binding used is a safe
//! `pub fn`, so `unsafe_code = "deny"` needs no exception.

use std::path::Path;

use log::debug;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSFileManager, NSOperatingSystemVersion, NSProcessInfo, NSString, NSURL};

use super::AppCandidate;

/// Launching through `open` uses the same detached spawn path as other
/// platforms, with no asynchronous completion handler.
const OPEN: &str = "/usr/bin/open";

/// `URLsForApplicationsToOpenURL:` was added in macOS 12; an older system
/// aborts on the unrecognized selector.
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
    // Launch Services documents no order; put the default first.
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
            // No "--": `open` could take it as a filename. The path is
            // absolute, so it cannot look like a flag.
            path.as_os_str().to_os_string(),
        ],
        detail: bundle,
        is_default,
        name,
        setting: None,
        working_dir: None,
    }
}
