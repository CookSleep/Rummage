//! 搜索历史：`search_history` 表，只存在本地的索引数据库中。
//! 同一查询只留一行，再次搜索时移到最前；记录时删去超出条数上限的旧记录。
//! `used_at` 取当前时间，但至少比已有的最大值大 1，同一毫秒内的两次搜索也能分出先后。
use rusqlite::params;

use crate::db::{Db, DbError, now_ms};

impl Db {
    /// 记下一次搜索；只保留最近的 `limit` 条
    pub fn add_history(&self, query: &str, limit: usize) -> Result<(), DbError> {
        let res = self.conn.unchecked_transaction().and_then(|tx| {
            tx.execute(
                "INSERT INTO search_history (query, used_at)
                 VALUES (?1, MAX(?2, COALESCE((SELECT MAX(used_at) FROM search_history), 0) + 1))
                 ON CONFLICT (query) DO UPDATE SET used_at = excluded.used_at",
                params![query, now_ms()],
            )?;
            tx.execute(
                "DELETE FROM search_history WHERE query NOT IN
                   (SELECT query FROM search_history ORDER BY used_at DESC LIMIT ?1)",
                [limit as i64],
            )?;
            tx.commit()
        });
        res.map_err(|err| self.err(err))
    }

    /// 最近的 `limit` 条，新的在前
    pub fn history(&self, limit: usize) -> Result<Vec<String>, DbError> {
        self.conn
            .prepare("SELECT query FROM search_history ORDER BY used_at DESC LIMIT ?1")
            .and_then(|mut stmt| stmt.query_map([limit as i64], |row| row.get(0))?.collect())
            .map_err(|err| self.err(err))
    }

    /// 不存在时什么也不做
    pub fn remove_history(&self, query: &str) -> Result<(), DbError> {
        self.conn
            .execute("DELETE FROM search_history WHERE query = ?1", [query])
            .map_err(|err| self.err(err))?;
        Ok(())
    }

    pub fn clear_history(&self) -> Result<(), DbError> {
        self.conn
            .execute("DELETE FROM search_history", [])
            .map_err(|err| self.err(err))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_recent_unique_queries() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        assert!(db.history(10).unwrap().is_empty());
        for q in ["柯基", "年终 总结", "旅行"] {
            db.add_history(q, 10).unwrap();
        }
        assert_eq!(db.history(10).unwrap(), ["旅行", "年终 总结", "柯基"]);
        // 再次搜索移到最前，不重复
        db.add_history("柯基", 10).unwrap();
        assert_eq!(db.history(10).unwrap(), ["柯基", "旅行", "年终 总结"]);
        assert_eq!(db.history(2).unwrap(), ["柯基", "旅行"]);
        // 超过上限时删去最早的
        db.add_history("发票", 2).unwrap();
        assert_eq!(db.history(10).unwrap(), ["发票", "柯基"]);

        db.remove_history("柯基").unwrap();
        db.remove_history("不存在").unwrap();
        assert_eq!(db.history(10).unwrap(), ["发票"]);
        db.clear_history().unwrap();
        assert!(db.history(10).unwrap().is_empty());
    }
}
