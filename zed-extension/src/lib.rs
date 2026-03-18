use std::{fs, path::Path};

use zed_extension_api::{
    self as zed, current_platform, download_file, github_release_by_tag_name, make_file_executable,
    serde_json::Value, set_language_server_installation_status, settings::LspSettings,
    Architecture, DownloadedFileType, GithubRelease, GithubReleaseAsset, LanguageServerId,
    LanguageServerInstallationStatus, Os, Result, Worktree,
};

const LANGUAGE_SERVER_ID: &str = "42-tools-lsp";
const SERVER_BINARY_NAME: &str = "forty-two-tools-lsp";
const GITHUB_REPOSITORY: &str = "EdizKeskin/42-tools-zed";
const SETTINGS_ENV_VAR: &str = "FORTY_TWO_TOOLS_SETTINGS_JSON";
const EXTENSION_VERSION: &str = env!("CARGO_PKG_VERSION");
const WINDOWS_UNSUPPORTED_MESSAGE: &str = "42 Tools currently supports macOS and Linux only.";

struct FortyTwoToolsExtension;

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstalledBinaries {
    version_dir: String,
    server_path: String,
}

impl FortyTwoToolsExtension {
    fn load_lsp_settings(worktree: &Worktree) -> Result<LspSettings> {
        LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree)
    }

    fn release_tag() -> String {
        format!("v{EXTENSION_VERSION}")
    }

    fn versioned_install_paths() -> InstalledBinaries {
        let version_dir = Self::release_tag();
        InstalledBinaries {
            server_path: format!("{version_dir}/{SERVER_BINARY_NAME}"),
            version_dir,
        }
    }

    fn configured_binary_path(settings: &LspSettings) -> Option<String> {
        settings
            .binary
            .as_ref()
            .and_then(|binary| binary.path.clone())
            .filter(|path| !path.trim().is_empty())
    }

    fn server_args(settings: &LspSettings) -> Vec<String> {
        settings
            .binary
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default()
    }

    fn server_env(settings: &LspSettings, worktree: &Worktree) -> Vec<(String, String)> {
        Self::build_server_env(worktree.shell_env(), settings)
    }

    fn build_server_env(
        mut env: Vec<(String, String)>,
        settings: &LspSettings,
    ) -> Vec<(String, String)> {
        if let Some(variables) = settings
            .binary
            .as_ref()
            .and_then(|binary| binary.env.as_ref())
        {
            for (key, value) in variables {
                Self::upsert_env(&mut env, key.clone(), value.clone());
            }
        }

        if let Some(lsp_settings) = settings.settings.as_ref() {
            Self::upsert_env(
                &mut env,
                SETTINGS_ENV_VAR.to_string(),
                lsp_settings.to_string(),
            );
        }

        env.sort_by(|left, right| left.0.cmp(&right.0));
        env
    }

    fn upsert_env(env: &mut Vec<(String, String)>, key: String, value: String) {
        if let Some((_, existing_value)) = env
            .iter_mut()
            .find(|(existing_key, _)| existing_key.as_str() == key.as_str())
        {
            *existing_value = value;
        } else {
            env.push((key, value));
        }
    }

    fn language_server_command_from_settings(
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
        settings: &LspSettings,
    ) -> Result<zed::Command> {
        if let Some(path) = Self::configured_binary_path(settings) {
            return Ok(zed::Command {
                command: path,
                args: Self::server_args(settings),
                env: Self::server_env(settings, worktree),
            });
        }

        if let Some(path) = worktree.which(SERVER_BINARY_NAME) {
            return Ok(zed::Command {
                command: path,
                args: Self::server_args(settings),
                env: Self::server_env(settings, worktree),
            });
        }

        let installed = Self::ensure_versioned_binaries(language_server_id)?;
        Ok(zed::Command {
            command: installed.server_path,
            args: Self::server_args(settings),
            env: Self::server_env(settings, worktree),
        })
    }

    fn ensure_versioned_binaries(
        language_server_id: &LanguageServerId,
    ) -> Result<InstalledBinaries> {
        match current_platform() {
            (Os::Windows, _) => Err(WINDOWS_UNSUPPORTED_MESSAGE.to_string()),
            (Os::Mac, Architecture::Aarch64)
            | (Os::Mac, Architecture::X8664)
            | (Os::Linux, Architecture::Aarch64)
            | (Os::Linux, Architecture::X8664) => {
                let installed = Self::versioned_install_paths();
                if Self::is_installed(&installed) {
                    return Ok(installed);
                }

                set_language_server_installation_status(
                    language_server_id,
                    &LanguageServerInstallationStatus::Downloading,
                );

                let result = Self::install_versioned_binaries(&installed);
                match result {
                    Ok(()) => {
                        set_language_server_installation_status(
                            language_server_id,
                            &LanguageServerInstallationStatus::None,
                        );
                        Ok(installed)
                    }
                    Err(error) => {
                        set_language_server_installation_status(
                            language_server_id,
                            &LanguageServerInstallationStatus::Failed(error.clone()),
                        );
                        Err(error)
                    }
                }
            }
            (os, arch) => Err(format!(
                "42 Tools does not have release assets for platform `{:?}` / `{:?}`",
                os, arch
            )),
        }
    }

    fn install_versioned_binaries(installed: &InstalledBinaries) -> Result<()> {
        Self::cleanup_old_installs(&installed.version_dir)?;

        let release = github_release_by_tag_name(GITHUB_REPOSITORY, &Self::release_tag()).map_err(
            |error| {
                format!(
                    "failed to load GitHub release `{}` for `{GITHUB_REPOSITORY}`: {error}",
                    Self::release_tag()
                )
            },
        )?;
        let asset_name = Self::release_asset_name()?;
        let asset = Self::find_release_asset(&release, asset_name)?;

        download_file(
            &asset.download_url,
            &installed.version_dir,
            DownloadedFileType::Zip,
        )
        .map_err(|error| format!("failed to download `{asset_name}`: {error}"))?;

        make_file_executable(&installed.server_path).map_err(|error| {
            format!("failed to mark `{SERVER_BINARY_NAME}` as executable: {error}")
        })?;
        if Self::is_installed(installed) {
            Ok(())
        } else {
            Err(format!(
                "release asset `{asset_name}` did not unpack the expected files into `{}`",
                installed.version_dir
            ))
        }
    }

    fn is_installed(installed: &InstalledBinaries) -> bool {
        Path::new(&installed.server_path).exists()
    }

    fn cleanup_old_installs(current_version_dir: &str) -> Result<()> {
        let entries = match fs::read_dir(".") {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };

        for entry in entries {
            let entry =
                entry.map_err(|error| format!("failed to inspect extension work dir: {error}"))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            if name == current_version_dir || !name.starts_with('v') {
                continue;
            }

            fs::remove_dir_all(&path).map_err(|error| {
                format!(
                    "failed to remove old bundled tool directory `{}`: {error}",
                    path.display()
                )
            })?;
        }

        Ok(())
    }

    fn release_asset_name() -> Result<&'static str> {
        match current_platform() {
            (Os::Mac, Architecture::Aarch64) => Ok("42-tools-mac-aarch64.zip"),
            (Os::Mac, Architecture::X8664) => Ok("42-tools-mac-x86_64.zip"),
            (Os::Linux, Architecture::Aarch64) => Ok("42-tools-linux-aarch64.zip"),
            (Os::Linux, Architecture::X8664) => Ok("42-tools-linux-x86_64.zip"),
            (Os::Windows, _) => Err(WINDOWS_UNSUPPORTED_MESSAGE.to_string()),
            (os, arch) => Err(format!(
                "42 Tools does not support platform `{:?}` / `{:?}`",
                os, arch
            )),
        }
    }

    fn find_release_asset<'a>(
        release: &'a GithubRelease,
        asset_name: &str,
    ) -> Result<&'a GithubReleaseAsset> {
        release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| {
                format!(
                    "GitHub release `{}` is missing asset `{asset_name}`",
                    release.version
                )
            })
    }
}

impl zed::Extension for FortyTwoToolsExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Err(format!(
                "unsupported language server id: {}",
                language_server_id
            ));
        }

        let settings = Self::load_lsp_settings(worktree)?;
        Self::language_server_command_from_settings(language_server_id, worktree, &settings)
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<Value>> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Ok(None);
        }

        Ok(Self::load_lsp_settings(worktree)?.initialization_options)
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<Value>> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Ok(None);
        }

        Ok(Self::load_lsp_settings(worktree)?.settings)
    }
}

zed::register_extension!(FortyTwoToolsExtension);

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn asset(name: &str) -> GithubReleaseAsset {
        GithubReleaseAsset {
            name: name.to_string(),
            download_url: format!("https://example.com/{name}"),
        }
    }

    #[test]
    fn versioned_paths_match_release_tag() {
        let paths = FortyTwoToolsExtension::versioned_install_paths();
        assert_eq!(paths.version_dir, "v0.1.4");
        assert_eq!(paths.server_path, "v0.1.4/forty-two-tools-lsp");
    }

    #[test]
    fn server_env_preserves_user_env_and_injects_defaults() {
        let settings = LspSettings {
            binary: None,
            initialization_options: None,
            settings: Some(zed::serde_json::json!({ "header": { "login": "marvin" } })),
        };

        let env = FortyTwoToolsExtension::build_server_env(
            vec![(
                "PATH".to_string(),
                "/home/edizk/.local/bin:/usr/bin".to_string(),
            )],
            &settings,
        );

        assert!(env
            .iter()
            .any(|(key, value)| { key == SETTINGS_ENV_VAR && value.contains("\"marvin\"") }));
        assert!(env
            .iter()
            .any(|(key, value)| key == "PATH" && value.contains("/home/edizk/.local/bin")));
    }

    #[test]
    fn release_asset_lookup_reports_missing_assets() {
        let release = GithubRelease {
            version: "v0.1.0".to_string(),
            assets: vec![asset("something-else.zip")],
        };

        let error =
            FortyTwoToolsExtension::find_release_asset(&release, "42-tools-mac-aarch64.zip")
                .expect_err("missing asset should fail");
        assert!(error.contains("42-tools-mac-aarch64.zip"));
    }

    #[test]
    fn cleanup_old_installs_ignores_regular_files() {
        let file_path = PathBuf::from("cleanup-old-installs-test.txt");
        fs::write(&file_path, "temporary").expect("test file written");
        FortyTwoToolsExtension::cleanup_old_installs("v0.1.0")
            .expect("cleanup should ignore files");
        fs::remove_file(file_path).expect("test file removed");
    }
}
