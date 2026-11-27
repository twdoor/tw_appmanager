use std::{
    cmp::Ordering,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use semver::Version;
use serde::Deserialize;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const API: &str = "https://api.github.com/repos/twdoor/twck/releases";
const RELEASE_PAGE: &str = "https://github.com/twdoor/twck/releases/tag";
const ASSET: &str = "Twck-x86_64.AppImage";
const PAGE_SIZE: usize = 100;

pub struct Update {
    pub version: Version,
    pub url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    state: String,
}

pub fn check(include_prereleases: bool) -> Result<Option<Update>> {
    let installed = Version::parse(VERSION)?;
    let mut latest = None;
    for page in 1.. {
        let url = format!("{API}?per_page={PAGE_SIZE}&page={page}");
        let output = Command::new("curl")
            .args([
                "--disable",
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--max-time",
                "8",
                "--max-filesize",
                "8388608",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--header",
                "Accept: application/vnd.github+json",
                "--user-agent",
                "Twck",
                &url,
            ])
            .stdin(Stdio::null())
            .output()
            .context("Could not run curl")?;
        ensure!(output.status.success(), "Could not reach GitHub Releases");
        let releases: Vec<Release> =
            serde_json::from_slice(&output.stdout).context("Invalid GitHub release data")?;
        let done = releases.len() < PAGE_SIZE;
        for release in releases {
            if let Some(update) = release.update(&installed, include_prereleases)
                && latest.as_ref().is_none_or(|previous: &Update| {
                    update.version.cmp_precedence(&previous.version).is_gt()
                })
            {
                latest = Some(update);
            }
        }
        if done {
            break;
        }
    }
    Ok(latest)
}

impl Release {
    fn update(self, installed: &Version, include_prereleases: bool) -> Option<Update> {
        let version = Version::parse(self.tag_name.strip_prefix('v')?).ok()?;
        if self.draft
            || (!include_prereleases && (self.prerelease || !version.pre.is_empty()))
            || version.cmp_precedence(installed) != Ordering::Greater
            || !self
                .assets
                .iter()
                .any(|asset| asset.name == ASSET && asset.state == "uploaded")
        {
            return None;
        }
        Some(Update {
            version,
            url: format!("{RELEASE_PAGE}/{}", self.tag_name),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> Release {
        Release {
            tag_name: tag.into(),
            draft: false,
            prerelease,
            assets: vec![Asset {
                name: ASSET.into(),
                state: "uploaded".into(),
            }],
        }
    }

    #[test]
    fn only_offers_newer_versions_in_the_selected_channel() -> Result<()> {
        let installed = Version::parse("1.2.9")?;
        assert!(
            release("v1.2.10", false)
                .update(&installed, false)
                .is_some()
        );
        assert!(
            release("v1.2.9+rebuilt", false)
                .update(&installed, true)
                .is_none()
        );
        assert!(release("v1.2.8", false).update(&installed, true).is_none());
        assert!(
            release("v1.3.0-rc.1", true)
                .update(&installed, false)
                .is_none()
        );
        assert!(release("v1.3.0", true).update(&installed, false).is_none());
        assert!(
            release("v1.3.0-rc.1", false)
                .update(&installed, false)
                .is_none()
        );
        assert_eq!(
            release("v1.3.0-rc.1", true)
                .update(&installed, true)
                .map(|update| update.url),
            Some(format!("{RELEASE_PAGE}/v1.3.0-rc.1"))
        );
        let installed = Version::parse("1.3.0-rc.1")?;
        assert!(release("v1.3.0", false).update(&installed, false).is_some());
        Ok(())
    }

    #[test]
    fn skips_drafts_invalid_tags_and_releases_without_uploaded_appimages() -> Result<()> {
        let installed = Version::parse("1.0.0")?;
        assert!(release("vnightly", true).update(&installed, true).is_none());
        let mut candidate = release("v2.0.0", false);
        candidate.draft = true;
        assert!(candidate.update(&installed, true).is_none());
        let mut candidate = release("v2.0.0", false);
        candidate.assets[0].state = "new".into();
        assert!(candidate.update(&installed, true).is_none());
        let mut candidate = release("v2.0.0", false);
        candidate.assets.clear();
        assert!(candidate.update(&installed, true).is_none());
        Ok(())
    }
}
