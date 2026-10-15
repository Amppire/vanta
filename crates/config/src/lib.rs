//! Secret-free portable profiles and offline extension lifecycle state.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Component;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROFILE_SCHEMA_VERSION: u16 = 1;
pub const SDK_VERSION: SdkVersion = SdkVersion { major: 0, minor: 5 };

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SdkVersion {
    pub major: u16,
    pub minor: u16,
}

impl SdkVersion {
    #[must_use]
    pub const fn is_supported(self) -> bool {
        self.major == 0 && (self.minor == 4 || self.minor == 5)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortableKeybinding {
    pub keys: String,
    pub command: String,
    #[serde(default)]
    pub when: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionSelection {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub schema_version: u16,
    pub name: String,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    #[serde(default)]
    pub keybindings: Vec<PortableKeybinding>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub extensions: Vec<ExtensionSelection>,
}

impl Profile {
    #[must_use]
    pub fn empty(name: impl Into<String>) -> Self {
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            name: name.into(),
            settings: BTreeMap::new(),
            keybindings: Vec::new(),
            theme: None,
            extensions: Vec::new(),
        }
    }

    /// Serializes a validated profile for portable export.
    ///
    /// # Errors
    /// Rejects secret-like settings, invalid identifiers, and serialization errors.
    pub fn export(&self) -> Result<String, ConfigError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| json_error(&error))
    }

    /// Imports and migrates a portable profile.
    ///
    /// # Errors
    /// Rejects malformed JSON, future schemas, secrets, and invalid entries.
    pub fn import(source: &str) -> Result<Self, ConfigError> {
        let mut profile: Self = serde_json::from_str(source).map_err(|error| json_error(&error))?;
        match profile.schema_version {
            0 => profile.schema_version = PROFILE_SCHEMA_VERSION,
            PROFILE_SCHEMA_VERSION => {}
            version => return Err(ConfigError::UnsupportedSchema(version)),
        }
        profile.validate()?;
        Ok(profile)
    }

    /// Validates that the profile is portable and contains no likely secrets.
    ///
    /// # Errors
    /// Returns the first invalid or secret-bearing field.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.name.trim().is_empty() {
            return Err(ConfigError::InvalidField("profile name".to_owned()));
        }
        for (key, value) in &self.settings {
            validate_setting(key, value)?;
        }
        for binding in &self.keybindings {
            if binding.keys.trim().is_empty() || !valid_identifier(&binding.command) {
                return Err(ConfigError::InvalidField("keybinding".to_owned()));
            }
        }
        for extension in &self.extensions {
            if !valid_identifier(&extension.id) || extension.version.trim().is_empty() {
                return Err(ConfigError::InvalidField("extension selection".to_owned()));
            }
        }
        Ok(())
    }
}

fn validate_setting(key: &str, value: &Value) -> Result<(), ConfigError> {
    let normalized = key.to_ascii_lowercase();
    if [
        "password",
        "secret",
        "token",
        "credential",
        "private_key",
        "api_key",
    ]
    .iter()
    .any(|word| normalized.contains(word))
    {
        return Err(ConfigError::SecretField(key.to_owned()));
    }
    match value {
        Value::Object(values) => {
            for (nested, value) in values {
                validate_setting(nested, value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_setting(key, value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum ExtensionPermission {
    ReadDocument,
    EditDocument,
    ReadWorkspace,
    LanguageServer,
    StatusItem,
    DeclarativeView,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionPackage {
    pub id: String,
    pub version: String,
    pub sdk: SdkVersion,
    pub entrypoint: PathBuf,
    #[serde(default)]
    pub permissions: BTreeSet<ExtensionPermission>,
}

impl ExtensionPackage {
    /// Reads an offline package manifest without executing package code.
    ///
    /// # Errors
    /// Rejects unreadable, malformed, incompatible, or invalid manifests.
    pub fn from_manifest(path: &Path) -> Result<Self, ConfigError> {
        let source = std::fs::read_to_string(path).map_err(|error| io_error(&error))?;
        let package: Self = serde_json::from_str(&source).map_err(|error| json_error(&error))?;
        package.validate()?;
        Ok(package)
    }

    /// Validates package identity and SDK compatibility.
    ///
    /// # Errors
    /// Rejects invalid identifiers, versions, entrypoints, and SDK versions.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !valid_identifier(&self.id) {
            return Err(ConfigError::InvalidField("extension id".to_owned()));
        }
        if self.version.trim().is_empty() {
            return Err(ConfigError::InvalidField("extension version".to_owned()));
        }
        if !self.sdk.is_supported() {
            return Err(ConfigError::UnsupportedSdk(self.sdk));
        }
        if self.entrypoint.as_os_str().is_empty()
            || self.entrypoint.is_absolute()
            || self
                .entrypoint
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(ConfigError::InvalidField("extension entrypoint".to_owned()));
        }
        Ok(())
    }
}

pub struct ProfileStore {
    profiles: BTreeMap<String, Profile>,
    active: String,
}

impl Default for ProfileStore {
    fn default() -> Self {
        let profile = Profile::empty("Default");
        Self {
            active: profile.name.clone(),
            profiles: BTreeMap::from([(profile.name.clone(), profile)]),
        }
    }
}

impl ProfileStore {
    #[must_use]
    pub fn active(&self) -> Option<&Profile> {
        self.profiles.get(&self.active)
    }

    /// Adds or replaces a validated profile.
    ///
    /// # Errors
    /// Returns an error when the profile is invalid or contains secret-like settings.
    pub fn insert(&mut self, profile: Profile) -> Result<(), ConfigError> {
        profile.validate()?;
        self.profiles.insert(profile.name.clone(), profile);
        Ok(())
    }

    /// Selects an existing profile.
    ///
    /// # Errors
    /// Returns an error when the profile does not exist.
    pub fn activate(&mut self, name: &str) -> Result<(), ConfigError> {
        if !self.profiles.contains_key(name) {
            return Err(ConfigError::ProfileNotFound);
        }
        name.clone_into(&mut self.active);
        Ok(())
    }

    /// Removes an inactive profile.
    ///
    /// # Errors
    /// Returns an error when the profile is active or does not exist.
    pub fn remove(&mut self, name: &str) -> Result<Profile, ConfigError> {
        if name == self.active {
            return Err(ConfigError::ActiveProfile);
        }
        self.profiles
            .remove(name)
            .ok_or(ConfigError::ProfileNotFound)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallOutcome {
    Installed,
    Updated,
    PermissionApprovalRequired(BTreeSet<ExtensionPermission>),
}

#[derive(Clone, Debug)]
pub struct InstalledExtension {
    pub active: ExtensionPackage,
    pub previous: Option<ExtensionPackage>,
    pub enabled: bool,
    pub approved_permissions: BTreeSet<ExtensionPermission>,
}

#[derive(Default)]
pub struct ExtensionManager {
    installed: BTreeMap<String, InstalledExtension>,
}

impl ExtensionManager {
    pub fn installed(&self) -> impl Iterator<Item = (&str, &InstalledExtension)> {
        self.installed
            .iter()
            .map(|(id, extension)| (id.as_str(), extension))
    }

    /// Installs or updates an offline package. Permission expansion never auto-activates.
    ///
    /// # Errors
    /// Rejects invalid or incompatible package metadata.
    pub fn install(&mut self, package: ExtensionPackage) -> Result<InstallOutcome, ConfigError> {
        package.validate()?;
        if let Some(current) = self.installed.get_mut(&package.id) {
            let expansion: BTreeSet<_> = package
                .permissions
                .difference(&current.approved_permissions)
                .copied()
                .collect();
            if !expansion.is_empty() {
                return Ok(InstallOutcome::PermissionApprovalRequired(expansion));
            }
            let previous = std::mem::replace(&mut current.active, package);
            current.previous = Some(previous);
            return Ok(InstallOutcome::Updated);
        }
        let approved_permissions = package.permissions.clone();
        self.installed.insert(
            package.id.clone(),
            InstalledExtension {
                active: package,
                previous: None,
                enabled: true,
                approved_permissions,
            },
        );
        Ok(InstallOutcome::Installed)
    }

    /// Approves a package update after the caller presents permission expansion to the user.
    ///
    /// # Errors
    /// Rejects invalid package metadata.
    pub fn approve_update(&mut self, package: ExtensionPackage) -> Result<(), ConfigError> {
        package.validate()?;
        let approved = package.permissions.clone();
        if let Some(current) = self.installed.get_mut(&package.id) {
            let previous = std::mem::replace(&mut current.active, package);
            current.previous = Some(previous);
            current.approved_permissions = approved;
            return Ok(());
        }
        Err(ConfigError::NotInstalled)
    }

    /// Enables or disables an installed extension without removing its package.
    ///
    /// # Errors
    /// Returns an error when the extension is not installed.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), ConfigError> {
        let extension = self
            .installed
            .get_mut(id)
            .ok_or(ConfigError::NotInstalled)?;
        extension.enabled = enabled;
        Ok(())
    }

    /// Swaps the active package with its retained previous version.
    ///
    /// # Errors
    /// Returns an error when the extension is missing or has no rollback package.
    pub fn rollback(&mut self, id: &str) -> Result<(), ConfigError> {
        let extension = self
            .installed
            .get_mut(id)
            .ok_or(ConfigError::NotInstalled)?;
        let previous = extension.previous.take().ok_or(ConfigError::NoRollback)?;
        let replaced = std::mem::replace(&mut extension.active, previous);
        extension.previous = Some(replaced);
        extension.approved_permissions = extension.active.permissions.clone();
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    InvalidField(String),
    SecretField(String),
    UnsupportedSchema(u16),
    UnsupportedSdk(SdkVersion),
    NotInstalled,
    NoRollback,
    ProfileNotFound,
    ActiveProfile,
    Io(String),
    Json(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField(field) => write!(formatter, "invalid {field}"),
            Self::SecretField(field) => {
                write!(formatter, "profile contains secret field `{field}`")
            }
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported profile schema {version}")
            }
            Self::UnsupportedSdk(version) => {
                write!(
                    formatter,
                    "unsupported SDK {}.{}",
                    version.major, version.minor
                )
            }
            Self::NotInstalled => formatter.write_str("extension is not installed"),
            Self::NoRollback => formatter.write_str("extension has no rollback package"),
            Self::ProfileNotFound => formatter.write_str("profile does not exist"),
            Self::ActiveProfile => formatter.write_str("active profile cannot be removed"),
            Self::Io(message) | Self::Json(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ConfigError {}

fn io_error(error: &std::io::Error) -> ConfigError {
    ConfigError::Io(error.to_string())
}

fn json_error(error: &serde_json::Error) -> ConfigError {
    ConfigError::Json(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn package(version: &str, permissions: &[ExtensionPermission]) -> ExtensionPackage {
        ExtensionPackage {
            id: "sample.formatter".to_owned(),
            version: version.to_owned(),
            sdk: SDK_VERSION,
            entrypoint: PathBuf::from("extension.wasm"),
            permissions: permissions.iter().copied().collect(),
        }
    }

    #[test]
    fn profile_round_trip_is_reproducible() {
        let mut profile = Profile::empty("Writing");
        profile
            .settings
            .insert("editor.tab_size".to_owned(), json!(4));
        profile.extensions.push(ExtensionSelection {
            id: "sample.formatter".to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
        });
        let exported = profile.export().unwrap();
        assert_eq!(Profile::import(&exported).unwrap(), profile);
        assert_eq!(
            Profile::import(&exported).unwrap().export().unwrap(),
            exported
        );
    }

    #[test]
    fn profile_export_rejects_nested_secrets() {
        let mut profile = Profile::empty("Unsafe");
        profile
            .settings
            .insert("service".to_owned(), json!({"api_token": "nope"}));
        assert!(matches!(profile.export(), Err(ConfigError::SecretField(_))));
    }

    #[test]
    fn previous_sdk_remains_compatible() {
        assert!(SdkVersion { major: 0, minor: 6 }.is_supported());
        assert!(!SdkVersion { major: 0, minor: 5 }.is_supported());
    }

    #[test]
    fn profiles_switch_without_mutating_each_other() {
        let mut profiles = ProfileStore::default();
        profiles.insert(Profile::empty("Rust")).unwrap();
        profiles.activate("Rust").unwrap();
        assert_eq!(profiles.active().unwrap().name, "Rust");
        assert!(matches!(
            profiles.remove("Rust"),
            Err(ConfigError::ActiveProfile)
        ));
    }

    #[test]
    fn updates_require_permission_expansion_approval_and_can_roll_back() {
        let mut manager = ExtensionManager::default();
        manager
            .install(package("1.0.0", &[ExtensionPermission::ReadDocument]))
            .unwrap();
        let update = package(
            "2.0.0",
            &[
                ExtensionPermission::ReadDocument,
                ExtensionPermission::EditDocument,
            ],
        );
        assert!(matches!(
            manager.install(update.clone()).unwrap(),
            InstallOutcome::PermissionApprovalRequired(_)
        ));
        manager.approve_update(update).unwrap();
        manager.rollback("sample.formatter").unwrap();
        assert_eq!(
            manager.installed().next().unwrap().1.active.version,
            "1.0.0"
        );
    }
}
