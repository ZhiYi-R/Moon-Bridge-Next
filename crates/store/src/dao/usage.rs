//! Usage DAO：用量记录写入与查询。

use rusqlite::params;
use rusqlite::types::ToSql;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::models::{UsageQuery, UsageRecord};
use crate::Database;

/// 用量汇总。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_cost: f64,
}

/// 按 provider 的消耗汇总（余额页「本地等值额度」对照上游配额用）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCost {
    pub provider_key: String,
    pub cost: f64,
    pub requests: i64,
}

const COLS: &str = "id,session_id,model,upstream_model,provider_key,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,reasoning_tokens,cost,status,error,latency_ms,ttft_ms,created_at";

fn row_to_usage(r: &rusqlite::Row) -> rusqlite::Result<UsageRecord> {
    Ok(UsageRecord {
        id: r.get(0)?,
        session_id: r.get(1)?,
        model: r.get(2)?,
        upstream_model: r.get(3)?,
        provider_key: r.get(4)?,
        input_tokens: r.get::<_, i64>(5)? as u32,
        output_tokens: r.get::<_, i64>(6)? as u32,
        cache_read_tokens: r.get::<_, i64>(7)? as u32,
        cache_write_tokens: r.get::<_, i64>(8)? as u32,
        reasoning_tokens: r.get::<_, i64>(9)? as u32,
        cost: r.get(10)?,
        status: r.get(11)?,
        error: r.get(12)?,
        latency_ms: r.get(13)?,
        ttft_ms: r.get(14)?,
        created_at: r.get(15)?,
    })
}

impl Database {
    /// 写入一条用量记录（id 为空时自动生成）。
    pub fn insert_usage(&self, rec: &UsageRecord) -> Result<()> {
        let conn = self.conn.lock();
        let id = if rec.id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            rec.id.clone()
        };
        let created = if rec.created_at > 0 {
            rec.created_at
        } else {
            crate::now_unix()
        };
        conn.execute(
            "INSERT INTO usage_records (id,session_id,model,upstream_model,provider_key,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,reasoning_tokens,cost,status,error,latency_ms,ttft_ms,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                id, rec.session_id, rec.model, rec.upstream_model, rec.provider_key,
                rec.input_tokens as i64, rec.output_tokens as i64,
                rec.cache_read_tokens as i64, rec.cache_write_tokens as i64,
                rec.reasoning_tokens as i64,
                rec.cost, rec.status, rec.error, rec.latency_ms, rec.ttft_ms, created
            ],
        )?;
        Ok(())
    }

    /// 按条件查询用量记录（按时间倒序）。
    pub fn query_usage(&self, q: &UsageQuery) -> Result<Vec<UsageRecord>> {
        let conn = self.conn.lock();
        let mut sql = format!("SELECT {COLS} FROM usage_records WHERE 1=1");
        let mut binds: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(m) = &q.model {
            sql.push_str(" AND model = ?");
            binds.push(Box::new(m.clone()));
        }
        if let Some(p) = &q.provider_key {
            sql.push_str(" AND provider_key = ?");
            binds.push(Box::new(p.clone()));
        }
        if let Some(s) = &q.status {
            sql.push_str(" AND status = ?");
            binds.push(Box::new(s.clone()));
        }
        if let Some(since) = q.since {
            sql.push_str(" AND created_at >= ?");
            binds.push(Box::new(since));
        }
        if let Some(until) = q.until {
            sql.push_str(" AND created_at <= ?");
            binds.push(Box::new(until));
        }
        // limit 钳制：非正数回退默认 100，无上限的 limit 会把整表读进内存；
        // 负 offset 在 SQLite 里静默当作 0，显式归一避免口径分歧。
        let limit = if q.limit > 0 { q.limit.min(500) } else { 100 };
        sql.push_str(" ORDER BY created_at DESC LIMIT ? OFFSET ?");
        binds.push(Box::new(limit));
        binds.push(Box::new(q.offset.max(0)));

        let params: Vec<&dyn ToSql> = binds.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), row_to_usage)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 全量用量汇总。
    pub fn usage_summary(&self) -> Result<UsageSummary> {
        self.usage_summary_range(None, None)
    }

    /// 时间范围内的用量汇总（created_at 秒级，含两端）。
    pub fn usage_summary_range(
        &self,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<UsageSummary> {
        let conn = self.conn.lock();
        let mut sql = String::from(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_write_tokens),0), COALESCE(SUM(reasoning_tokens),0), COALESCE(SUM(cost),0) FROM usage_records WHERE 1=1",
        );
        let mut binds: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(s) = since {
            sql.push_str(" AND created_at >= ?");
            binds.push(Box::new(s));
        }
        if let Some(u) = until {
            sql.push_str(" AND created_at <= ?");
            binds.push(Box::new(u));
        }
        let params: Vec<&dyn ToSql> = binds.iter().map(|b| b.as_ref()).collect();
        Ok(conn.query_row(&sql, params.as_slice(), |r| {
            Ok(UsageSummary {
                requests: r.get(0)?,
                input_tokens: r.get(1)?,
                output_tokens: r.get(2)?,
                cache_read_tokens: r.get(3)?,
                cache_write_tokens: r.get(4)?,
                reasoning_tokens: r.get(5)?,
                total_cost: r.get(6)?,
            })
        })?)
    }

    /// 按 provider 汇总 since（秒级，含端点）以来的消耗；provider_key 为空的记录无对照对象，剔除。
    pub fn usage_cost_by_provider(&self, since: Option<i64>) -> Result<Vec<ProviderCost>> {
        let conn = self.conn.lock();
        let mut sql = String::from(
            "SELECT provider_key, COALESCE(SUM(cost),0), COUNT(*) FROM usage_records WHERE provider_key <> ''",
        );
        let mut binds: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(s) = since {
            sql.push_str(" AND created_at >= ?");
            binds.push(Box::new(s));
        }
        sql.push_str(" GROUP BY provider_key");
        let params: Vec<&dyn ToSql> = binds.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |r| {
            Ok(ProviderCost {
                provider_key: r.get(0)?,
                cost: r.get(1)?,
                requests: r.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}
