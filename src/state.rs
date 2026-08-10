use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

const STATE_VERSION: u32 = 1;
const STATE_FILE_NAME: &str = "delivery-state.json";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DeliveryState {
    #[serde(default = "state_version")]
    pub version: u32,
    #[serde(default)]
    pub deliveries: BTreeMap<String, DeliveryRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeliveryRecord {
    pub issue_id: String,
    pub issue_date: String,
    pub accepted_at: String,
}

fn state_version() -> u32 {
    STATE_VERSION
}

impl DeliveryState {
    pub fn load(data_dir: &Path) -> Result<Self, String> {
        let path = state_path(data_dir);
        if !path.exists() {
            return Ok(Self {
                version: STATE_VERSION,
                deliveries: BTreeMap::new(),
            });
        }
        let bytes = fs::read(&path).map_err(|error| format!("读取状态文件失败：{error}"))?;
        let state = match serde_json::from_slice::<Self>(&bytes) {
            Ok(state) => state,
            Err(error) => {
                let backup = corrupt_backup_path(&path);
                fs::rename(&path, &backup).map_err(|rename_error| {
                    format!("状态文件损坏（{error}），且备份失败：{rename_error}")
                })?;
                return Err(format!(
                    "状态文件损坏，已移动到 {}，自动推送暂停",
                    backup.display()
                ));
            }
        };
        if state.version != STATE_VERSION {
            return Err(format!(
                "状态文件版本 {} 不受支持，当前版本为 {STATE_VERSION}，自动推送暂停",
                state.version
            ));
        }
        Ok(state)
    }

    pub fn contains(&self, target_key: &str, issue_id: &str) -> bool {
        self.deliveries
            .get(target_key)
            .is_some_and(|record| record.issue_id == issue_id)
    }

    pub fn mark_accepted(
        &mut self,
        data_dir: &Path,
        target_key: String,
        issue_id: String,
        issue_date: String,
    ) -> Result<(), String> {
        let previous = self.deliveries.insert(
            target_key.clone(),
            DeliveryRecord {
                issue_id,
                issue_date,
                accepted_at: Utc::now().to_rfc3339(),
            },
        );
        if let Err(error) = self.save(data_dir) {
            match previous {
                Some(record) => {
                    self.deliveries.insert(target_key, record);
                }
                None => {
                    self.deliveries.remove(&target_key);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    fn save(&self, data_dir: &Path) -> Result<(), String> {
        fs::create_dir_all(data_dir).map_err(|error| format!("创建状态目录失败：{error}"))?;
        let path = state_path(data_dir);
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|error| format!("序列化状态失败：{error}"))?;
        let mut temp = tempfile::NamedTempFile::new_in(data_dir)
            .map_err(|error| format!("创建状态临时文件失败：{error}"))?;
        temp.write_all(&bytes)
            .map_err(|error| format!("写入状态临时文件失败：{error}"))?;
        temp.as_file()
            .sync_all()
            .map_err(|error| format!("刷新状态临时文件失败：{error}"))?;
        temp.persist(&path)
            .map_err(|error| format!("原子替换状态文件失败：{}", error.error))?;
        Ok(())
    }
}

pub fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE_NAME)
}

fn corrupt_backup_path(path: &Path) -> PathBuf {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    path.with_file_name(format!("{STATE_FILE_NAME}.corrupt-{timestamp}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_and_reloads_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = DeliveryState::load(dir.path()).unwrap();
        state
            .mark_accepted(
                dir.path(),
                "onebot11|bot|group".to_string(),
                "issue".to_string(),
                "2026-08-10".to_string(),
            )
            .unwrap();

        let loaded = DeliveryState::load(dir.path()).unwrap();
        assert!(loaded.contains("onebot11|bot|group", "issue"));

        state
            .mark_accepted(
                dir.path(),
                "onebot11|bot|group".to_string(),
                "issue-2".to_string(),
                "2026-08-11".to_string(),
            )
            .unwrap();
        let loaded = DeliveryState::load(dir.path()).unwrap();
        assert!(loaded.contains("onebot11|bot|group", "issue-2"));
    }

    #[test]
    fn corrupt_state_is_backed_up_and_blocks_loading() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(state_path(dir.path()), b"not json").unwrap();
        let error = DeliveryState::load(dir.path()).unwrap_err();
        assert!(error.contains("自动推送暂停"));
        assert!(!state_path(dir.path()).exists());
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains("corrupt"))
        );
    }

    #[test]
    fn rejects_unknown_future_version_without_treating_it_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(state_path(dir.path()), br#"{"version":2,"deliveries":{}}"#).unwrap();

        let error = DeliveryState::load(dir.path()).unwrap_err();

        assert!(error.contains("版本 2 不受支持"));
        assert!(state_path(dir.path()).exists());
    }

    #[test]
    fn persisted_state_has_explicit_version_and_only_delivery_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = DeliveryState::load(dir.path()).unwrap();
        state
            .mark_accepted(
                dir.path(),
                "qq-official|app|group-openid".to_string(),
                "issue-1".to_string(),
                "2026-08-10".to_string(),
            )
            .unwrap();

        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(state_path(dir.path())).unwrap()).unwrap();
        assert_eq!(value["version"], 1);
        let text = value.to_string();
        assert!(!text.contains("base64"));
        assert!(!text.contains("markdown"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("bot_instance"));
    }

    #[test]
    fn write_failure_rolls_back_in_memory_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let invalid_data_dir = dir.path().join("not-a-directory");
        fs::write(&invalid_data_dir, b"file").unwrap();
        let mut state = DeliveryState::default();

        let error = state
            .mark_accepted(
                &invalid_data_dir,
                "onebot11|bot|group".to_string(),
                "issue".to_string(),
                "2026-08-10".to_string(),
            )
            .unwrap_err();

        assert!(error.contains("状态目录"));
        assert!(!state.contains("onebot11|bot|group", "issue"));
    }

    #[test]
    fn creates_missing_state_directory() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("nested").join("ai-news");
        let mut state = DeliveryState::load(&data_dir).unwrap();

        state
            .mark_accepted(
                &data_dir,
                "onebot11|bot|group".to_string(),
                "issue".to_string(),
                "2026-08-10".to_string(),
            )
            .unwrap();

        assert!(state_path(&data_dir).is_file());
    }
}
