//! Regression coverage for the two audit reports. All identities are synthetic.
use crate::{cloud::CloudSession, credentials::*, controller::{Controller, PropValue}, profile::Profile};
use serde_json::json;

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("miac-remediation-{}-{n}", std::process::id()));
        std::fs::create_dir(&p).unwrap(); Self(p)
    }
    fn creds(&self) -> Credentials { Credentials::isolated(&self.0) }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn session(id: &str) -> CloudSession {
    CloudSession { username: id.into(), user_id: id.into(),
        ssecurity: "MDEyMzQ1Njc4OWFiY2RlZg==".into(), service_token: format!("token-{id}").into(), ..Default::default() }
}
fn device(did: &str) -> DeviceInfo {
    DeviceInfo { did: did.into(), model: Some(crate::miot::EXPECT_MODEL.into()), ..Default::default() }
}

#[test]
fn credential_constructor_has_no_implicit_import_directories() {
    let scratch = Scratch::new();
    let creds = Credentials::new(&scratch.0);
    assert_eq!(creds.search_dirs().collect::<Vec<_>>(), vec![scratch.0.as_path()]);
}

#[cfg(windows)]
#[test]
fn login_commit_binds_all_three_records_and_clears_previous_sensor() {
    let scratch = Scratch::new(); let creds = scratch.creds();
    let sensor = ThermometerInfo { did: "sensor-a".into(), ..Default::default() };
    creds.write_login(&session("account-a"), &device("device-a"), Some(&sensor)).unwrap();
    let state = creds.read_login().unwrap();
    assert_eq!((state.session.user_id.as_str(), state.device.did.as_str(), state.thermometer.did.as_str()), ("account-a", "device-a", "sensor-a"));
    creds.write_login(&session("account-b"), &device("device-b"), None).unwrap();
    assert_eq!(creds.read_device().unwrap().did, "device-b");
    assert_eq!(creds.read_session().unwrap().user_id, "account-b");
    assert!(creds.read_thermometer().is_none());
    let raw = std::fs::read(scratch.0.join(FILE_LOGIN)).unwrap();
    assert!(crate::dpapi::is_current_format(&raw));
    assert!(!String::from_utf8_lossy(&raw).contains("token-account-b"));
    assert!(creds.protection_status().active_files_protected());
}

#[cfg(windows)]
#[test]
fn failed_atomic_replacement_preserves_the_complete_old_account() {
    use std::os::windows::fs::OpenOptionsExt;
    let scratch = Scratch::new(); let creds = scratch.creds();
    let sensor = ThermometerInfo { did: "sensor-a".into(), ..Default::default() };
    creds.write_login(&session("account-a"), &device("device-a"), Some(&sensor)).unwrap();
    let guard = std::fs::OpenOptions::new().read(true).share_mode(3).open(scratch.0.join(FILE_LOGIN)).unwrap();
    assert!(creds.write_login(&session("account-b"), &device("device-b"), None).is_err());
    let old = creds.read_login().unwrap();
    assert_eq!(old.session.user_id, "account-a");
    assert_eq!(old.device.did, "device-a");
    assert_eq!(old.thermometer.did, "sensor-a");
    drop(guard);
    assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 1, "failed temporary writes must be removed");
}

#[cfg(windows)]
#[test]
fn cancellation_at_the_commit_boundary_keeps_the_old_account() {
    let scratch = Scratch::new(); let creds = scratch.creds();
    creds.write_login(&session("old-account"), &device("old-device"), None).unwrap();
    assert!(creds.write_login_if(&session("new-account"), &device("new-device"), None, || false).is_err());
    assert_eq!(creds.read_login().unwrap().session.user_id, "old-account");
    assert_eq!(creds.read_login().unwrap().device.did, "old-device");
    assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 1);
}

#[test]
fn damaged_transaction_never_resurrects_legacy_device_or_account() {
    let scratch = Scratch::new(); let creds = scratch.creds();
    std::fs::write(scratch.0.join(FILE_DEVICE), br#"{"did":"old-device"}"#).unwrap();
    std::fs::write(scratch.0.join(FILE_LOGIN), b"broken transaction").unwrap();
    assert!(creds.read_login().is_none());
    assert!(creds.read_device().is_none());
    assert!(creds.read_session().is_none());
    assert!(creds.read_thermometer().is_none());
}

#[test]
fn oversized_credentials_are_rejected_and_bad_setting_types_preserve_other_preferences() {
    let scratch = Scratch::new(); let creds = scratch.creds();
    let file = std::fs::File::create(scratch.0.join(FILE_DEVICE)).unwrap();
    file.set_len(crate::io_limits::CREDENTIAL_BYTES as u64 + 1).unwrap(); drop(file);
    assert!(creds.read_device().is_none());
    std::fs::write(scratch.0.join(FILE_SETTINGS), br#"{"dark":1,"transport":2,"migrated":true,"presets":[26,27,28]}"#).unwrap();
    let settings = crate::settings::Settings::load(&creds);
    assert_eq!(settings.transport, 2); assert!(settings.migrated); assert!(!settings.dark);
    assert_eq!(settings.presets, vec![26.0,27.0,28.0]);
}

#[test]
fn every_bundled_fault_contract_handles_normal_and_missing_readings() {
    let specs = crate::profile::bundled_specs().as_object().unwrap();
    assert!(specs.len() >= 46);
    for (model, spec) in specs {
        let profile = if model == crate::miot::EXPECT_MODEL { Profile::legacy() } else { Profile::parse(model, spec).unwrap() };
        let name = if model == crate::miot::EXPECT_MODEL { "faultValue" } else { "fault" };
        if let Some(prop) = profile.properties.get(name) {
            let value = if prop.format == "string" { json!("0,0,0") } else { json!(0) };
            assert!(crate::controller::profile_fault_info(&profile, &[(name, PropValue::Ok(value))]).clear, "{model}");
        } else {
            assert!(crate::controller::profile_fault_info(&profile, &[]).text.contains("未提供"));
        }
        assert!(crate::controller::profile_fault_value(&profile, &[]).is_none());
    }
    assert!(!crate::miot::fault_info(Some(i64::MIN)).clear);
}

#[test]
fn invalid_device_destinations_and_cookie_values_are_rejected() {
    for did in ["../x", "a/b", "1?x=y", "1#fragment", "", "1\n"] {
        assert!(!valid_did(did));
    }
    for host in ["example.org", "8.8.8.8", "192.168.1.2:80", "255.255.255.255", "::1"] {
        assert!(crate::miio::local_address(host).is_err(), "{host}");
    }
    assert!(crate::miio::local_address("192.168.1.2").is_ok());
    let mut invalid = session("fake-id"); invalid.service_token = "token; userId=other".into();
    assert!(!invalid.ready());
    let cloud = crate::cloud::CloudClient::new(session("fake"), None);
    assert!(cloud.miio_call("1/../../x", "get_properties", &json!([])).is_err());
}

#[test]
fn range_only_fan_profiles_expose_zero_as_the_automatic_choice() {
    for model in ["viomi.aircondition.v6", "viomi.aircondition.v7", "viomi.aircondition.v8", "viomi.aircondition.v9"] {
        let profile = Profile::parse(model, &crate::profile::bundled_specs()[model]).unwrap();
        assert!(profile.properties["fanLevel"].choices.iter().any(|(v, label)| *v == 0 && label == "自动"));
        assert!(profile.validate("fanLevel", &json!(0)).is_ok());
    }
}

#[cfg(windows)]
#[test]
fn account_change_on_the_same_device_invalidates_a_bound_controller() {
    let scratch = Scratch::new(); let creds = scratch.creds();
    creds.write_login(&session("account-a"), &device("same-device"), None).unwrap();
    let mut ctrl = Controller::new(creds.clone(), crate::Transport::Cloud);
    ctrl.init_transport().unwrap(); assert!(ctrl.credentials_current());
    creds.write_login(&session("account-b"), &device("same-device"), None).unwrap();
    assert!(!ctrl.credentials_current());
    assert!(ctrl.write_prop("on", json!(true)).unwrap_err().0.contains("凭据已变化"));
}
