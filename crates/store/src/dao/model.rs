//! Model 与 Offer DAO。

use rusqlite::params;
use serde_json::Value;

use crate::error::Result;
use crate::models::{ModelDef, Offer};
use crate::Database;

fn opt_json(s: Option<String>) -> Option<Value> {
    s.and_then(|x| serde_json::from_str(&x).ok())
}

fn row_to_model(r: &rusqlite::Row) -> rusqlite::Result<ModelDef> {
    Ok(ModelDef {
        slug: r.get(0)?,
        display_name: r.get(1)?,
        context_window: r.get(2)?,
        modalities: opt_json(r.get(3)?),
        reasoning_levels: opt_json(r.get(4)?),
        extra: opt_json(r.get(5)?).unwrap_or(Value::Null),
    })
}

impl Database {
    /// 列出全部模型定义。
    pub fn list_models(&self) -> Result<Vec<ModelDef>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT slug,display_name,context_window,modalities_json,reasoning_levels_json,extra_json FROM models ORDER BY slug",
        )?;
        let rows = stmt.query_map([], row_to_model)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 按 slug 获取模型。
    pub fn get_model(&self, slug: &str) -> Result<Option<ModelDef>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT slug,display_name,context_window,modalities_json,reasoning_levels_json,extra_json FROM models WHERE slug = ?1",
        )?;
        let mut rows = stmt.query_map(params![slug], row_to_model)?;
        match rows.next() {
            Some(Ok(m)) => Ok(Some(m)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 插入或更新模型定义。
    pub fn upsert_model(&self, m: &ModelDef) -> Result<()> {
        let conn = self.conn.lock();
        let j = |v: &Option<Value>| v.as_ref().map(|x| x.to_string());
        let extra = if m.extra.is_null() {
            "{}".to_string()
        } else {
            m.extra.to_string()
        };
        conn.execute(
            "INSERT INTO models (slug,display_name,context_window,modalities_json,reasoning_levels_json,extra_json)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(slug) DO UPDATE SET
                display_name=excluded.display_name, context_window=excluded.context_window,
                modalities_json=excluded.modalities_json, reasoning_levels_json=excluded.reasoning_levels_json,
                extra_json=excluded.extra_json",
            params![
                m.slug, m.display_name, m.context_window,
                j(&m.modalities), j(&m.reasoning_levels), extra
            ],
        )?;
        Ok(())
    }

    /// 删除模型。
    pub fn delete_model(&self, slug: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM models WHERE slug = ?1", params![slug])?;
        conn.execute("DELETE FROM offers WHERE model_slug = ?1", params![slug])?;
        Ok(())
    }

    /// 列出某 provider 的模型报价。
    pub fn list_offers(&self, provider_key: &str) -> Result<Vec<Offer>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT provider_key,model_slug,pricing_json,endpoint_protocol FROM offers WHERE provider_key = ?1 ORDER BY model_slug",
        )?;
        let rows = stmt.query_map(params![provider_key], |r| {
            Ok(Offer {
                provider_key: r.get(0)?,
                model_slug: r.get(1)?,
                pricing: opt_json(r.get(2)?),
                endpoint_protocol: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 插入或更新报价。
    pub fn upsert_offer(&self, o: &Offer) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO offers (provider_key,model_slug,pricing_json,endpoint_protocol) VALUES (?1,?2,?3,?4)
             ON CONFLICT(provider_key,model_slug) DO UPDATE SET pricing_json=excluded.pricing_json, endpoint_protocol=excluded.endpoint_protocol",
            params![o.provider_key, o.model_slug, o.pricing.as_ref().map(|v| v.to_string()), o.endpoint_protocol],
        )?;
        Ok(())
    }

    /// 插入报价（仅当该 (provider, model) 报价不存在时）；返回是否真正插入。
    pub fn insert_offer_if_absent(&self, o: &Offer) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
"INSERT OR IGNORE INTO offers (provider_key,model_slug,pricing_json,endpoint_protocol) VALUES (?1,?2,?3,?4)",
params![o.provider_key, o.model_slug, o.pricing.as_ref().map(|v| v.to_string()), o.endpoint_protocol],
        )?;
        Ok(n > 0)
    }

    /// 删除报价。
    pub fn delete_offer(&self, provider_key: &str, model_slug: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM offers WHERE provider_key = ?1 AND model_slug = ?2",
            params![provider_key, model_slug],
        )?;
        Ok(())
    }
}
