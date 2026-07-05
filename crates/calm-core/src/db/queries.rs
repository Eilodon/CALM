use std::collections::HashMap;

use rusqlite::Connection;

pub fn batch_callees(
    conn: &Connection,
    nodes: &[&str],
) -> rusqlite::Result<HashMap<String, Vec<(String, String)>>> {
    if nodes.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; nodes.len()].join(",");
    let sql = format!(
        "SELECT from_symbol, to_symbol, edge_confidence \
         FROM call_edges WHERE from_symbol IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = nodes
        .iter()
        .map(|s| s as &dyn rusqlite::types::ToSql)
        .collect();

    let mut result: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    for row in rows {
        let (from_s, to_s, conf) = row?;
        result.entry(from_s).or_default().push((to_s, conf));
    }
    Ok(result)
}

pub fn batch_callers(
    conn: &Connection,
    nodes: &[&str],
) -> rusqlite::Result<HashMap<String, Vec<(String, String)>>> {
    if nodes.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; nodes.len()].join(",");
    let sql = format!(
        "SELECT to_symbol, from_symbol, edge_confidence \
         FROM call_edges WHERE to_symbol IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = nodes
        .iter()
        .map(|s| s as &dyn rusqlite::types::ToSql)
        .collect();

    let mut result: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    for row in rows {
        let (to_s, from_s, conf) = row?;
        result.entry(to_s).or_default().push((from_s, conf));
    }
    Ok(result)
}
