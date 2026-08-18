use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 全局执行上下文作用域 (Scope)
#[derive(Debug, Default, Clone)]
pub struct Scope {
    pub values: Arc<tokio::sync::RwLock<HashMap<String, Value>>>,
    pub auto_counter: Arc<AtomicUsize>,
}

impl Scope {
    pub fn new() -> Self {
        Self {
            values: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            auto_counter: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn set(&self, id: String, value: Value) {
        let mut map = self.values.write().await;
        map.insert(id, value);
    }

    pub async fn get(&self, id: &str) -> Option<Value> {
        let map = self.values.read().await;
        map.get(id).cloned()
    }

    pub fn next_index_id(&self) -> String {
        let idx = self.auto_counter.fetch_add(1, Ordering::SeqCst);
        format!("${}", idx)
    }

    /// 解析变量引用，如 $fetch_user.name 或 $0[1].data
    pub fn resolve_value<'a>(
        &'a self,
        input: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            match input {
                Value::String(s) => {
                    if s.starts_with('$') {
                        if let Some(val) = self.resolve_path(s).await {
                            return val;
                        }
                    }
                    // 字符串内嵌替换 (例如 "search $search_topic in web")
                    if s.contains('$') {
                        let map = self.values.read().await;
                        let mut rendered = s.clone();
                        for (k, v) in map.iter() {
                            let token = if k.starts_with('$') {
                                k.clone()
                            } else {
                                format!("${}", k)
                            };
                            if rendered.contains(&token) {
                                let replacement = match v {
                                    Value::String(inner_s) => inner_s.clone(),
                                    other => other.to_string(),
                                };
                                rendered = rendered.replace(&token, &replacement);
                            }
                        }
                        return Value::String(rendered);
                    }
                    Value::String(s.clone())
                }
                Value::Array(arr) => {
                    let mut out = Vec::with_capacity(arr.len());
                    for item in arr {
                        out.push(self.resolve_value(item).await);
                    }
                    Value::Array(out)
                }
                Value::Object(obj) => {
                    let mut out = serde_json::Map::with_capacity(obj.len());
                    for (k, v) in obj {
                        out.insert(k.clone(), self.resolve_value(v).await);
                    }
                    Value::Object(out)
                }
                other => other.clone(),
            }
        })
    }

    /// 路径提取器：支持 $id, $id.a.b, $id[0].name, $0[1]
    async fn resolve_path(&self, expr: &str) -> Option<Value> {
        let clean = expr.trim();
        let map = self.values.read().await;

        let (root_id, remainder) = split_root_and_path(clean);

        // 尝试两种 root: 原样 (带$) 和去$
        let root_val = map
            .get(&root_id)
            .or_else(|| map.get(root_id.trim_start_matches('$')))?;

        if remainder.is_empty() {
            return Some(root_val.clone());
        }

        extract_nested_value(root_val, &remainder)
    }
}

fn split_root_and_path(expr: &str) -> (String, String) {
    let mut root = String::new();
    let mut remainder = String::new();
    let mut in_remainder = false;

    for ch in expr.chars() {
        if !in_remainder {
            if ch == '.' || ch == '[' {
                in_remainder = true;
                if ch != '.' {
                    remainder.push(ch);
                }
            } else {
                root.push(ch);
            }
        } else {
            remainder.push(ch);
        }
    }

    (root, remainder)
}

fn extract_nested_value(root: &Value, path: &str) -> Option<Value> {
    let mut current = root.clone();
    let normalized = path.replace('[', ".").replace(']', "");
    let segments: Vec<&str> = normalized.split('.').filter(|s| !s.is_empty()).collect();

    for seg in segments {
        if let Ok(idx) = seg.parse::<usize>() {
            if let Value::Array(arr) = current {
                current = arr.get(idx)?.clone();
            } else {
                return None;
            }
        } else if let Value::Object(obj) = current {
            current = obj.get(seg)?.clone();
        } else {
            return None;
        }
    }

    Some(current)
}
