use super::*;
use std::fs;
use std::path::Path;

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    fn stage_read_only_disc(
        &self,
        source: &Path,
        expected: &crate::launch::xemu::FileIdentity,
    ) -> XemuResult<PathBuf> {
        let machine_root = self.state_environment.hdd.parent().ok_or_else(|| {
            XemuBridgeError::BadState("managed Xbox HDD path has no parent".into())
        })?;
        let media_root = machine_root.join("removable-media");
        fs::create_dir_all(&media_root)?;
        let metadata = fs::symlink_metadata(&media_root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(XemuBridgeError::BadState(
                "managed Xbox removable-media root is not a plain directory".into(),
            ));
        }
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("xiso")
            .to_ascii_lowercase();
        let staged = media_root.join(format!(
            "disc-{}.{}",
            ulid::Ulid::generate().to_string().to_ascii_lowercase(),
            extension
        ));
        crate::launch::xemu::copy_verified(source, &staged, expected)?;
        let mut permissions = fs::metadata(&staged)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&staged, permissions)?;
        Ok(staged)
    }

    pub(super) fn get_rom_info(&self) -> XemuResult<Value> {
        let content = self
            .current_disc
            .as_ref()
            .ok_or_else(|| XemuBridgeError::BadState("no Xbox disc is currently mounted".into()))?;
        let (size, sha1) = sha1_regular_file(content)?;
        Ok(json!({
            "system":"xbox", "adapter":"xemu-rust-qmp-gdb",
            "name":content.file_name().and_then(|name| name.to_str()).unwrap_or(""),
            "path":content.display().to_string(), "sha1":sha1, "size":size,
            "media_type":content.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase(),
            "machine_inputs": self.machine_identity.value(),
        }))
    }

    pub(super) fn change_media(&mut self, params: &Value) -> XemuResult<Value> {
        let device = params
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or("dvd");
        if !matches!(device, "dvd" | "dvd0" | "disc") {
            return Err(XemuBridgeError::BadParams(format!(
                "unsupported Xbox media device: {device}; valid: dvd"
            )));
        }
        if self.is_running()? {
            return Err(XemuBridgeError::BadState(
                "Xbox change_media requires a frozen VM; pause first".into(),
            ));
        }
        let previous = self
            .current_disc
            .as_ref()
            .map(|path| path.display().to_string());
        if params
            .get("eject")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.qmp.execute("xemu-emucap-eject-disc", None)?;
            self.current_disc = None;
            self.state_media_identity_cache = None;
            return Ok(json!({
                "status":"completed", "state":"frozen", "device":"dvd",
                "action":"eject", "previous":previous, "current":null
            }));
        }

        let path = PathBuf::from(required_str(params, "path")?);
        if !path.is_absolute() {
            return Err(XemuBridgeError::BadParams(
                "Xbox replacement disc path must be absolute".into(),
            ));
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "iso" | "xiso") {
            return Err(XemuBridgeError::BadParams(
                "Xbox replacement media must be a raw .iso or .xiso file".into(),
            ));
        }
        let (size, sha1) = sha1_regular_file(&path)?;
        if let Some(expected) = params.get("expected_sha1").and_then(Value::as_str) {
            if !expected.eq_ignore_ascii_case(&sha1) {
                return Err(XemuBridgeError::BadParams(format!(
                    "replacement disc SHA-1 mismatch: expected {expected}, got {sha1}"
                )));
            }
        }
        let source_identity = crate::launch::xemu::identity_for_regular_file(&path)?;
        let staged = self.stage_read_only_disc(&path, &source_identity)?;
        let (staged_size, staged_sha1) = sha1_regular_file(&staged)?;
        if staged_size != size || staged_sha1 != sha1 {
            let _ = fs::remove_file(&staged);
            return Err(XemuBridgeError::BadState(
                "Xbox source disc changed while creating its generation copy".into(),
            ));
        }
        if let Err(error) = self.qmp.execute(
            "xemu-emucap-load-disc",
            Some(json!({"path":staged.display().to_string()})),
        ) {
            let _ = fs::remove_file(&staged);
            return Err(error.into());
        }
        self.current_disc = Some(staged.clone());
        self.state_media_identity_cache = None;
        Ok(json!({
            "status":"completed", "state":"frozen", "device":"dvd", "action":"mount",
            "previous":previous,
            "current":{"path":staged.display().to_string(), "source_path":path.display().to_string(), "sha1":sha1, "sha256":source_identity.sha256, "size":size, "readonly":true, "binding":"generation_copy"}
        }))
    }
}
