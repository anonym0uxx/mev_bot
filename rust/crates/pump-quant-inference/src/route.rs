//! Model-ID routing by request family (probe_mg_v3 candidate serving). OFF by default.
//!
//! Production today sends no `"model"` field (the server's single served model answers). That stays the default:
//! [`ModelRoute::Base`] leaves every request body BYTE-IDENTICAL to [`crate::request_body`] (tested). A candidate run selects
//! [`ModelRoute::Candidate`], which names one served model id per request FAMILY (entry vs management) — the only selector.
//! In candidate mode a missing id, an empty id, or an id the server does not list is a named startup refusal; there is
//! no fallback to the base model.
//!
//! Configuration (read once at startup, [`ModelRoute::from_env`]):
//!   `PQ_MODEL_ROUTE` unset or `base`      -> Base (production; nothing changes)
//!   `PQ_MODEL_ROUTE=candidate`            -> Candidate; requires `PQ_MODEL_ID_ENTRY` and `PQ_MODEL_ID_MGMT`
//!   any other value                       -> refusal `model_route_unknown`

use std::collections::BTreeSet;

/// Which question a request asks. The ONLY input to model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestFamily {
    /// New-entry decision.
    Entry,
    /// Held-position management decision.
    Management,
}

/// How requests choose a served model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRoute {
    /// No `"model"` field (production default).
    Base,
    /// Name an adapter (served model id) per family.
    Candidate {
        /// Served model id for entry requests.
        entry: String,
        /// Served model id for management requests.
        management: String,
    },
}

/// Named refusals. Each one stops startup in candidate mode; none falls back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// `PQ_MODEL_ROUTE` is neither unset/`base` nor `candidate`.
    Unknown(String),
    /// Candidate mode without the id for this family.
    MissingId(RequestFamily),
    /// The two families name the same id (adapters must be distinct).
    SameIdForBothFamilies(String),
    /// The server's `/v1/models` does not list this id.
    NotServed(String),
}

impl ModelRoute {
    /// Parse from explicit values (testable; `from_env` reads the process environment).
    ///
    /// # Errors
    /// See [`RouteError`].
    pub fn parse(
        route: Option<&str>,
        entry: Option<&str>,
        mgmt: Option<&str>,
    ) -> Result<Self, RouteError> {
        match route.map(str::trim) {
            None | Some("") | Some("base") => Ok(ModelRoute::Base),
            Some("candidate") => {
                let e = entry
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or(RouteError::MissingId(RequestFamily::Entry))?;
                let m = mgmt
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or(RouteError::MissingId(RequestFamily::Management))?;
                if e == m {
                    return Err(RouteError::SameIdForBothFamilies(e.to_string()));
                }
                Ok(ModelRoute::Candidate {
                    entry: e.to_string(),
                    management: m.to_string(),
                })
            }
            Some(other) => Err(RouteError::Unknown(other.to_string())),
        }
    }

    /// Read `PQ_MODEL_ROUTE`, `PQ_MODEL_ID_ENTRY`, `PQ_MODEL_ID_MGMT`.
    ///
    /// # Errors
    /// See [`RouteError`].
    pub fn from_env() -> Result<Self, RouteError> {
        let g = |k: &str| std::env::var(k).ok();
        Self::parse(
            g("PQ_MODEL_ROUTE").as_deref(),
            g("PQ_MODEL_ID_ENTRY").as_deref(),
            g("PQ_MODEL_ID_MGMT").as_deref(),
        )
    }

    /// The model id for `family` (None in Base mode = omit the field).
    #[must_use]
    pub fn model_for(&self, family: RequestFamily) -> Option<&str> {
        match self {
            ModelRoute::Base => None,
            ModelRoute::Candidate { entry, management } => Some(match family {
                RequestFamily::Entry => entry.as_str(),
                RequestFamily::Management => management.as_str(),
            }),
        }
    }

    /// Candidate mode: every routed id must be listed by the server. Base mode: always Ok.
    ///
    /// # Errors
    /// [`RouteError::NotServed`] naming the first missing id.
    pub fn verify_served(&self, served_ids: &BTreeSet<String>) -> Result<(), RouteError> {
        for f in [RequestFamily::Entry, RequestFamily::Management] {
            if let Some(id) = self.model_for(f) {
                if !served_ids.contains(id) {
                    return Err(RouteError::NotServed(id.to_string()));
                }
            }
        }
        Ok(())
    }
}

/// Candidate request profile answer budget: the evaluation's 128-token acceptance limit (gen_eval_v3a / probe_mg3 gates), so the
/// candidate is served under exactly the budget it was evaluated under. Production (Base) keeps its own unchanged body.
pub const CANDIDATE_MAX_TOKENS: u32 = 128;

/// The chat body for `family` under `route`.
/// * Base: [`crate::request_body`] unchanged (production profile; no `"model"`).
/// * Candidate: same messages / temperature 0 / non-streaming / `enable_thinking=false` / no stop strings, plus the pinned
///   candidate profile: `"model"` = the family's adapter id, `"max_tokens"` = [`CANDIDATE_MAX_TOKENS`], `"return_token_ids"` = true
///   (readout only; does not change generation). The probe_mg3 evaluator builds byte-identical bodies (golden parity test).
#[must_use]
pub fn routed_request_body(
    route: &ModelRoute,
    family: RequestFamily,
    system: &str,
    user: &str,
) -> serde_json::Value {
    let mut b = crate::request_body(system, user);
    if let Some(id) = route.model_for(family) {
        b["model"] = serde_json::json!(id);
        b["max_tokens"] = serde_json::json!(CANDIDATE_MAX_TOKENS);
        b["return_token_ids"] = serde_json::json!(true);
    }
    b
}

/// Parse an OpenAI `/v1/models` body into the set of served ids.
///
/// # Errors
/// `"models_body_unparseable"` when the body is not the expected JSON.
pub fn served_ids(models_body: &str) -> Result<BTreeSet<String>, &'static str> {
    let v: serde_json::Value =
        serde_json::from_str(models_body).map_err(|_| "models_body_unparseable")?;
    let data = v["data"].as_array().ok_or("models_body_unparseable")?;
    Ok(data
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_is_the_default_and_leaves_the_body_byte_identical() {
        for r in [None, Some(""), Some("base"), Some("  base ")] {
            let route = ModelRoute::parse(r, Some("x"), Some("y")).unwrap();
            assert_eq!(route, ModelRoute::Base);
            for f in [RequestFamily::Entry, RequestFamily::Management] {
                let a = routed_request_body(&route, f, "SYS", "USER");
                assert_eq!(
                    serde_json::to_string(&a).unwrap(),
                    serde_json::to_string(&crate::request_body("SYS", "USER")).unwrap()
                );
                assert!(a.get("model").is_none());
            }
        }
    }

    #[test]
    fn candidate_selects_solely_by_family() {
        let route = ModelRoute::parse(
            Some("candidate"),
            Some("probe-mg3-entry"),
            Some("probe-mg3-mgmt"),
        )
        .unwrap();
        let e = routed_request_body(&route, RequestFamily::Entry, "S", "U");
        let m = routed_request_body(&route, RequestFamily::Management, "S", "U");
        assert_eq!(e["model"], "probe-mg3-entry");
        assert_eq!(m["model"], "probe-mg3-mgmt");
        // candidate profile = production body with exactly model / max_tokens 128 / return_token_ids changed or added
        assert_eq!(e["max_tokens"], 128);
        assert_eq!(e["return_token_ids"], true);
        let mut e2 = e.clone();
        let o = e2.as_object_mut().unwrap();
        o.remove("model");
        o.remove("return_token_ids");
        o.insert(
            "max_tokens".into(),
            crate::request_body("S", "U")["max_tokens"].clone(),
        );
        assert_eq!(e2, crate::request_body("S", "U"));
        assert!(e.get("stop").is_none(), "no stop strings in either profile");
        // same prompts, different family -> different model; same family, different prompt -> same model
        let m2 = routed_request_body(&route, RequestFamily::Management, "OTHER", "TEXT");
        assert_eq!(m2["model"], m["model"]);
    }

    #[test]
    fn candidate_refuses_missing_empty_same_or_unknown() {
        assert_eq!(
            ModelRoute::parse(Some("candidate"), None, Some("m")),
            Err(RouteError::MissingId(RequestFamily::Entry))
        );
        assert_eq!(
            ModelRoute::parse(Some("candidate"), Some("e"), Some("  ")),
            Err(RouteError::MissingId(RequestFamily::Management))
        );
        assert_eq!(
            ModelRoute::parse(Some("candidate"), Some("a"), Some("a")),
            Err(RouteError::SameIdForBothFamilies("a".into()))
        );
        assert_eq!(
            ModelRoute::parse(Some("adapters"), Some("e"), Some("m")),
            Err(RouteError::Unknown("adapters".into()))
        );
    }

    #[test]
    fn candidate_requires_every_id_to_be_served_no_fallback() {
        let route = ModelRoute::parse(
            Some("candidate"),
            Some("probe-mg3-entry"),
            Some("probe-mg3-mgmt"),
        )
        .unwrap();
        let body = r#"{"object":"list","data":[{"id":"probe-mg3-base"},{"id":"probe-mg3-entry"}]}"#;
        let ids = served_ids(body).unwrap();
        assert_eq!(
            route.verify_served(&ids),
            Err(RouteError::NotServed("probe-mg3-mgmt".into()))
        );
        let ok = served_ids(r#"{"data":[{"id":"probe-mg3-base"},{"id":"probe-mg3-entry"},{"id":"probe-mg3-mgmt"}]}"#).unwrap();
        assert_eq!(route.verify_served(&ok), Ok(()));
        assert_eq!(ModelRoute::Base.verify_served(&BTreeSet::new()), Ok(()));
        assert_eq!(served_ids("not json"), Err("models_body_unparseable"));
    }
}
