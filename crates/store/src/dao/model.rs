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
        max_output_tokens: r.get(3)?,
        modalities: opt_json(r.get(4)?),
        reasoning_levels: opt_json(r.get(5)?),
        extra: opt_json(r.get(6)?).unwrap_or(Value::Null),
    })
}

impl Database {
    /// 列出全部模型定义。
    pub fn list_models(&self) -> Result<Vec<ModelDef>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT slug,display_name,context_window,max_output_tokens,modalities_json,reasoning_levels_json,extra_json FROM models ORDER BY slug",
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
            "SELECT slug,display_name,context_window,max_output_tokens,modalities_json,reasoning_levels_json,extra_json FROM models WHERE slug = ?1",
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
            "INSERT INTO models (slug,display_name,context_window,max_output_tokens,modalities_json,reasoning_levels_json,extra_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(slug) DO UPDATE SET
                display_name=excluded.display_name, context_window=excluded.context_window,
                max_output_tokens=excluded.max_output_tokens,
                modalities_json=excluded.modalities_json, reasoning_levels_json=excluded.reasoning_levels_json,
                extra_json=excluded.extra_json",
            params![
                m.slug, m.display_name, m.context_window, m.max_output_tokens,
                j(&m.modalities), j(&m.reasoning_levels), extra
            ],
        )?;
        Ok(())
    }

    /// 删除模型（级联删除其 offers 与 model 维度插件绑定；指向该 slug 的
    /// routes 保留——model_slug 是自由名，模型重建后路由恢复可用）。事务化。
    pub fn delete_model(&self, slug: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM models WHERE slug = ?1", params![slug])?;
        tx.execute("DELETE FROM offers WHERE model_slug = ?1", params![slug])?;
        tx.execute(
            "DELETE FROM plugin_bindings WHERE scope = 'model' AND scope_key = ?1",
            params![slug],
        )?;
        tx.commit()?;
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

    /// 按 slug 给所有同模型报价回填定价：仅更新 `pricing_json IS NULL` 的行，
    /// 用户手填过的定价一律不动。返回被回填的行数。
    ///
    /// 背景：目录导入的定价落在 models.dev 的 provider 命名空间下，而用户本地
    /// provider 用自己的 key；Provider 页勾选绑定还会先建出 `pricing: null` 的行。
    /// 两边一叠，用户 key 名下的报价就永久没定价。这里只填空，不覆盖。
    pub fn backfill_offer_pricing(&self, model_slug: &str, pricing: &Value) -> Result<usize> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE offers SET pricing_json = ?1 WHERE model_slug = ?2 AND pricing_json IS NULL",
            params![pricing.to_string(), model_slug],
        )?;
        Ok(n)
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
