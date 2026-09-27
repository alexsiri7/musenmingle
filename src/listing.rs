//! `GET /v1/events` query parameters and pagination cursors (pure; the SQL
//! is in `repo::list_events`).

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Europe::London;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::enrich::output::{FORMAT_TAGS, GOOD_FOR, MEDIUM_TAGS};
use crate::model::Category;
use crate::normalise::london_to_utc;
use crate::search::Search;

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 100;
pub const DEFAULT_RADIUS_KM: f64 = 5.0;
pub const MAX_RADIUS_KM: f64 = 100.0;
/// Walking pace and detour factor for straight-line distances; the same
/// numbers as `WALK_KMH` / `DETOUR` in `src/map.mjs` (its "≈ N min walk").
pub const WALK_KMH: f64 = 5.0;
pub const WALK_DETOUR: f64 = 1.3;
/// `within_walk_min=` bounds.
pub const MAX_WALK_MIN: u32 = 60;

/// The straight-line radius for "within `minutes` walk": every event whose
/// `src/map.mjs` label rounds to at most `minutes` (hence the half minute).
pub fn walk_radius_km(minutes: u32) -> f64 {
    (f64::from(minutes) + 0.5) / 60.0 * WALK_KMH / WALK_DETOUR
}
/// Most ids one `ids=` filter may name.
pub const MAX_IDS: usize = 100;

/// `within_hours=` default and bounds for `at=now`.
pub const DEFAULT_WITHIN_HOURS: i64 = 3;
pub const MAX_WITHIN_HOURS: i64 = 24;
/// In `at=` mode the listing's lower bound (`from`, `$1` in SQL) is this far
/// before the requested instant: a loose, index-friendly bound that every
/// event still on at that instant satisfies (an all-day event's stored end
/// is London midnight of its last day, at most 25 hours before it ends).
/// The exact test is `repo::live_sql`, which recovers the instant as
/// `$1 + LIVE_LOOKBACK_HOURS`.
pub const LIVE_LOOKBACK_HOURS: i64 = 48;
/// An event with a start time but no end counts as still on for this long
/// after it starts (we rarely know when a talk finishes).
pub const LIVE_GRACE_MINUTES: i64 = 60;

/// Mean Earth radius used for haversine distances.
pub const EARTH_RADIUS_KM: f64 = 6371.0088;

/// A parsed `GET /v1/events` request.
#[derive(Debug, Clone, PartialEq)]
pub struct EventQuery {
    pub filter: EventFilter,
    /// Area filter (`near=` + `radius_km=`): only events within the radius.
    /// Applies to every sort, not only `nearest`.
    pub near: Option<Near>,
    pub order: EventOrder,
    /// The sort the client asked for when it could not be applied
    /// (`sort=nearest` without `near` falls back to `soonest`).
    pub fell_back_from: Option<Sort>,
    pub limit: i64,
    /// Also return tag counts for the other filters (`facets=true`).
    pub facets: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventFilter {
    /// Events still running at or after this instant (London midnight of `from`).
    pub from: Option<DateTime<Utc>>,
    /// Events starting before this instant (London midnight after `to`).
    pub until: Option<DateTime<Utc>>,
    /// Empty = any category.
    pub categories: Vec<Category>,
    pub free_only: bool,
    /// Source keys; empty = any. An event matches if any of its listings
    /// comes from one of them.
    pub sources: Vec<String>,
    /// Only these events (`ids=<uuid>,<uuid>`); empty = no restriction.
    pub ids: Vec<Uuid>,
    /// Medium tags (`medium=`, repeatable): events with ANY of them.
    pub mediums: Vec<String>,
    /// Format tags (`format=`, repeatable): events with ANY of them.
    pub formats: Vec<String>,
    /// `good_for=` (repeatable): events with ANY of them.
    pub good_for: Vec<String>,
    /// Time-of-day / day-of-week bucket (`when=`).
    pub when: Option<When>,
    /// Free events and GBP events whose lowest price is at most this;
    /// unknown prices are excluded.
    pub price_max: Option<Decimal>,
    /// Full-text search (`q=`, see [`crate::search`]).
    pub search: Option<Search>,
    /// A quick pick (`pick=`), judged against a London date (today).
    pub pick: Option<PickFilter>,
    /// `at=now|today`: only events still on at this instant (and, via
    /// `until`, starting before the end of the window). `from` is then this
    /// instant minus [`LIVE_LOOKBACK_HOURS`].
    pub live_at: Option<DateTime<Utc>>,
}

/// `pick=`: the home page's quick-pick chips as listing filters. Each is
/// one SQL predicate (`repo::pick_sql`), so a chip's count and the listing
/// it opens always agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Today (London): timed and starting at 17:00 or later, or an untimed
    /// event running today tagged `late opening`.
    Tonight,
    /// Starting within the next 7 London days (today included) and an
    /// opening: an exhibition, `is_opening` (AI, quoted from the listing),
    /// the `opening` format tag, or a title saying private view, opening
    /// reception, opening night, preview evening, launch or "PV".
    Openings,
    /// Runs on more than one London day and its last day is within the next
    /// 7 (the `sort=ending` notion of an end; not yet ended).
    LastChance,
    /// Workshops, the `hands_on` format tag, or a title saying class,
    /// course, drop-in, life drawing, masterclass or workshop.
    HandsOn,
}

impl Pick {
    pub const ALL: [Pick; 4] = [
        Pick::Tonight,
        Pick::Openings,
        Pick::LastChance,
        Pick::HandsOn,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Pick::Tonight => "tonight",
            Pick::Openings => "openings",
            Pick::LastChance => "last_chance",
            Pick::HandsOn => "hands_on",
        }
    }

    pub fn parse(s: &str) -> Option<Pick> {
        Pick::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

/// A [`Pick`] and the London date it is relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickFilter {
    pub pick: Pick,
    pub today: NaiveDate,
}

/// `at=`: "happening now or starting soon" windows (London time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// Still on now, or starting within `within_hours` (default 3).
    Now,
    /// Still on now, or starting later today (before London midnight).
    Today,
}

/// Add a vocabulary tag to `list` (deduped), or explain why it is invalid.
fn push_tag(list: &mut Vec<String>, name: &str, value: &str, vocab: &[&str]) -> Result<(), String> {
    if !vocab.contains(&value) {
        return Err(format!(
            "unknown {name} {value:?} (one of: {})",
            vocab.join(", ")
        ));
    }
    if !list.iter().any(|v| v == value) {
        list.push(value.to_string());
    }
    Ok(())
}

/// `when=` buckets, judged on Europe/London local time. An event starting
/// at London midnight is untimed (a date-only listing, e.g. an exhibition
/// run); an untimed event tagged `late opening` counts as open in the
/// evening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Timed and starts at 18:00 or later, or untimed with late opening.
    Evening,
    /// Timed, starts Monday–Friday between 17:30 and 20:30 inclusive, or
    /// untimed with late opening.
    AfterWork,
    /// Its London date range, clipped to the `from`/`to` window, includes a
    /// Saturday or Sunday.
    Weekend,
    /// Untimed, or timed and starts before 18:00.
    Daytime,
}

impl When {
    pub const ALL: [When; 4] = [When::Evening, When::AfterWork, When::Weekend, When::Daytime];

    pub fn as_str(self) -> &'static str {
        match self {
            When::Evening => "evening",
            When::AfterWork => "after_work",
            When::Weekend => "weekend",
            When::Daytime => "daytime",
        }
    }
}

/// `sort=` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// Starting soonest: `starts_at` ascending (ongoing events first).
    Soonest,
    /// Closest to `near` first; needs `near`.
    Nearest,
    /// Last chance: effective end ascending, only events not yet ended.
    Ending,
    /// Just added: first time Muse & Mingle saw the event, newest first.
    Added,
    /// Surprise me: a random order, reshuffled each London day.
    Surprise,
    /// Best match for `q` first; needs `q`. The default when `q` is set.
    Relevance,
}

impl Sort {
    pub const ALL: [Sort; 6] = [
        Sort::Soonest,
        Sort::Nearest,
        Sort::Ending,
        Sort::Added,
        Sort::Surprise,
        Sort::Relevance,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Sort::Soonest => "soonest",
            Sort::Nearest => "nearest",
            Sort::Ending => "ending",
            Sort::Added => "added",
            Sort::Surprise => "surprise",
            Sort::Relevance => "relevance",
        }
    }

    /// The label shown in the "Sort" control.
    pub fn label(self) -> &'static str {
        match self {
            Sort::Soonest => "Starting soonest",
            Sort::Nearest => "Closest to me",
            Sort::Ending => "Last chance",
            Sort::Added => "Just added",
            Sort::Surprise => "Surprise me",
            Sort::Relevance => "Best match",
        }
    }

    pub fn parse(s: &str) -> Option<Sort> {
        Sort::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// The sort of a listing and where the previous page stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum EventOrder {
    ByStart {
        after: Option<(DateTime<Utc>, Uuid)>,
    },
    ByDistance {
        near: Near,
        after: Option<(f64, Uuid)>,
    },
    /// By effective end ([`crate::repo`] `EFFECTIVE_END`), only events
    /// ending after `now`.
    ByEnd {
        now: DateTime<Utc>,
        after: Option<(DateTime<Utc>, Uuid)>,
    },
    /// By first-seen time, newest first.
    ByAdded {
        after: Option<(DateTime<Utc>, Uuid)>,
    },
    /// By `md5(id || seed)`: a shuffle that is stable for a London day. The
    /// seed comes from the cursor on later pages, so a walk that crosses
    /// midnight keeps its order.
    Shuffled {
        seed: NaiveDate,
        after: Option<Uuid>,
    },
    /// Best match for `q` first (`ts_rank`, then id).
    ByRelevance { after: Option<(f64, Uuid)> },
}

impl EventOrder {
    pub fn sort(&self) -> Sort {
        match self {
            EventOrder::ByStart { .. } => Sort::Soonest,
            EventOrder::ByDistance { .. } => Sort::Nearest,
            EventOrder::ByEnd { .. } => Sort::Ending,
            EventOrder::ByAdded { .. } => Sort::Added,
            EventOrder::Shuffled { .. } => Sort::Surprise,
            EventOrder::ByRelevance { .. } => Sort::Relevance,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Near {
    pub lat: f64,
    pub lng: f64,
    pub radius_km: f64,
}

/// Inclusive lat/lng box containing every point within `radius_km` of the
/// centre (a cheap, index-friendly prefilter before the exact distance).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingBox {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lng: f64,
    pub max_lng: f64,
}

impl Near {
    /// Ignores the antimeridian, which London never gets near.
    pub fn bounding_box(&self) -> BoundingBox {
        let angle = self.radius_km / EARTH_RADIUS_KM;
        let dlat = angle.to_degrees();
        // Widest longitude offset of the circle (it is reached north of the
        // centre's parallel, so `angle / cos(lat)` would be slightly short).
        let dlng = match angle.sin() / self.lat.to_radians().cos() {
            s if s < 1.0 => s.asin().to_degrees(),
            _ => 180.0,
        };
        BoundingBox {
            min_lat: (self.lat - dlat).max(-90.0),
            max_lat: (self.lat + dlat).min(90.0),
            min_lng: self.lng - dlng,
            max_lng: self.lng + dlng,
        }
    }
}

/// The position of the last event on a page. Encoded as an opaque string;
/// only valid with the same filters and sort it was issued for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cursor {
    Start(DateTime<Utc>, Uuid),
    Distance(f64, Uuid),
    End(DateTime<Utc>, Uuid),
    Added(DateTime<Utc>, Uuid),
    /// The shuffle's seed day and the last id (its key is `md5(id || seed)`).
    Shuffle(NaiveDate, Uuid),
    /// The search rank and the last id.
    Relevance(f64, Uuid),
}

impl Cursor {
    pub fn encode(&self) -> String {
        let plain = match self {
            Cursor::Start(t, id) => format!("s:{}:{id}", t.timestamp_micros()),
            Cursor::Distance(d, id) => format!("d:{:016x}:{id}", d.to_bits()),
            Cursor::End(t, id) => format!("e:{}:{id}", t.timestamp_micros()),
            Cursor::Added(t, id) => format!("a:{}:{id}", t.timestamp_micros()),
            Cursor::Shuffle(day, id) => format!("r:{}:{id}", day.format("%Y%m%d")),
            Cursor::Relevance(r, id) => format!("q:{:016x}:{id}", r.to_bits()),
        };
        plain.bytes().map(|b| format!("{b:02x}")).collect()
    }

    pub fn decode(s: &str) -> Option<Cursor> {
        if s.len() % 2 != 0 || !s.is_ascii() {
            return None;
        }
        let bytes = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        let plain = String::from_utf8(bytes).ok()?;
        let mut parts = plain.splitn(3, ':');
        let (kind, pos, id) = (parts.next()?, parts.next()?, parts.next()?);
        let id = Uuid::parse_str(id).ok()?;
        let micros = || DateTime::from_timestamp_micros(pos.parse().ok()?);
        match kind {
            "s" => Some(Cursor::Start(micros()?, id)),
            "d" => Some(Cursor::Distance(
                f64::from_bits(u64::from_str_radix(pos, 16).ok()?),
                id,
            )),
            "e" => Some(Cursor::End(micros()?, id)),
            "a" => Some(Cursor::Added(micros()?, id)),
            "r" => Some(Cursor::Shuffle(
                NaiveDate::parse_from_str(pos, "%Y%m%d").ok()?,
                id,
            )),
            "q" => Some(Cursor::Relevance(
                f64::from_bits(u64::from_str_radix(pos, 16).ok()?),
                id,
            )),
            _ => None,
        }
    }
}

/// Today's date in London (the "Surprise me" seed).
pub fn london_today(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&London).date_naive()
}

/// Parse a raw query string (`category` may repeat). Errors are messages
/// for the client.
pub fn parse_query(raw: &str) -> Result<EventQuery, String> {
    parse_query_at(raw, Utc::now())
}

/// [`parse_query`] with the clock given (`ending` hides events that ended
/// before `now`; `surprise` shuffles by `now`'s London date; `at=` is
/// relative to it).
pub fn parse_query_at(raw: &str, now: DateTime<Utc>) -> Result<EventQuery, String> {
    let mut filter = EventFilter::default();
    let (mut at, mut within_hours) = (None, None);
    let (mut from, mut to) = (None, None);
    let (mut near, mut radius_km, mut walk_min) = (None, None, None);
    let (mut limit, mut cursor) = (None, None);
    let mut facets = false;
    let mut sort = None;
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        match key.as_ref() {
            "medium" => push_tag(&mut filter.mediums, "medium", &value, MEDIUM_TAGS)?,
            "format" => push_tag(&mut filter.formats, "format", &value, FORMAT_TAGS)?,
            "good_for" => push_tag(&mut filter.good_for, "good_for", &value, GOOD_FOR)?,
            "facets" => {
                facets = match value.as_ref() {
                    "true" => true,
                    "false" => false,
                    _ => return Err("facets must be true or false".into()),
                }
            }
            "pick" => {
                let pick = Pick::parse(&value).ok_or_else(|| {
                    let all: Vec<&str> = Pick::ALL.iter().map(|p| p.as_str()).collect();
                    format!("unknown pick {value:?} (one of: {})", all.join(", "))
                })?;
                filter.pick = Some(PickFilter {
                    pick,
                    today: now.with_timezone(&London).date_naive(),
                });
            }
            "sort" => {
                sort = Some(Sort::parse(&value).ok_or_else(|| {
                    let all: Vec<&str> = Sort::ALL.iter().map(|s| s.as_str()).collect();
                    format!("unknown sort {value:?} (one of: {})", all.join(", "))
                })?)
            }
            "q" => filter.search = Search::parse(&value)?,
            "at" => {
                at = Some(match value.as_ref() {
                    "now" => At::Now,
                    "today" => At::Today,
                    _ => return Err("at must be now or today".into()),
                })
            }
            "within_hours" => {
                let h: i64 = value
                    .parse()
                    .map_err(|_| "within_hours must be an integer".to_string())?;
                if !(1..=MAX_WITHIN_HOURS).contains(&h) {
                    return Err(format!(
                        "within_hours must be between 1 and {MAX_WITHIN_HOURS}"
                    ));
                }
                within_hours = Some(h);
            }
            "from" => from = Some(parse_date("from", &value)?),
            "to" => to = Some(parse_date("to", &value)?),
            "category" => {
                let c = Category::ALL
                    .into_iter()
                    .find(|c| c.as_str() == value)
                    .ok_or_else(|| format!("unknown category {value:?}"))?;
                if !filter.categories.contains(&c) {
                    filter.categories.push(c);
                }
            }
            "free" => {
                filter.free_only = match value.as_ref() {
                    "true" => true,
                    "false" => false,
                    _ => return Err("free must be true or false".into()),
                }
            }
            "when" => {
                filter.when = Some(
                    When::ALL
                        .into_iter()
                        .find(|w| w.as_str() == value)
                        .ok_or_else(|| format!("unknown when {value:?}"))?,
                )
            }
            "price_max" => {
                let err = || "price_max must be a non-negative number".to_string();
                let max: Decimal = value.parse().map_err(|_| err())?;
                if max < Decimal::ZERO {
                    return Err(err());
                }
                filter.price_max = Some(max);
            }
            "source" => {
                if !is_source_key(&value) {
                    return Err(format!("invalid source {value:?}"));
                }
                if !filter.sources.iter().any(|s| *s == value) {
                    filter.sources.push(value.into_owned());
                }
            }
            "ids" => {
                for part in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    let id = Uuid::parse_str(part)
                        .map_err(|_| format!("ids must be comma-separated UUIDs, got {part:?}"))?;
                    if !filter.ids.contains(&id) {
                        filter.ids.push(id);
                    }
                }
                if filter.ids.len() > MAX_IDS {
                    return Err(format!("ids may name at most {MAX_IDS} events"));
                }
                if filter.ids.is_empty() {
                    return Err("ids must name at least one event".into());
                }
            }
            "near" => near = Some(parse_near(&value)?),
            "radius_km" => {
                let r: f64 = value
                    .parse()
                    .map_err(|_| "radius_km must be a number".to_string())?;
                if !(r > 0.0 && r <= MAX_RADIUS_KM) {
                    return Err(format!("radius_km must be > 0 and <= {MAX_RADIUS_KM}"));
                }
                radius_km = Some(r);
            }
            "within_walk_min" => {
                let m: u32 = value
                    .parse()
                    .map_err(|_| "within_walk_min must be a whole number".to_string())?;
                if !(1..=MAX_WALK_MIN).contains(&m) {
                    return Err(format!(
                        "within_walk_min must be between 1 and {MAX_WALK_MIN}"
                    ));
                }
                walk_min = Some(m);
            }
            "limit" => {
                let l: i64 = value
                    .parse()
                    .map_err(|_| "limit must be an integer".to_string())?;
                if !(1..=MAX_LIMIT).contains(&l) {
                    return Err(format!("limit must be between 1 and {MAX_LIMIT}"));
                }
                limit = Some(l);
            }
            "cursor" => {
                cursor = Some(Cursor::decode(&value).ok_or_else(|| "invalid cursor".to_string())?)
            }
            other => return Err(format!("unknown parameter {other:?}")),
        }
    }
    if let (Some(f), Some(t)) = (from, to) {
        if f > t {
            return Err("from must not be after to".into());
        }
    }
    filter.from = from.map(london_midnight);
    filter.until = to.and_then(|t| t.succ_opt()).map(london_midnight);
    match at {
        None if within_hours.is_some() => return Err("within_hours requires at=now".into()),
        None => {}
        Some(_) if from.is_some() || to.is_some() || filter.when.is_some() => {
            return Err("at cannot be combined with from, to or when".into());
        }
        Some(At::Today) if within_hours.is_some() => {
            return Err("within_hours requires at=now".into());
        }
        Some(at) => {
            filter.live_at = Some(now);
            filter.from = Some(now - chrono::Duration::hours(LIVE_LOOKBACK_HOURS));
            filter.until = Some(match at {
                At::Now => {
                    now + chrono::Duration::hours(within_hours.unwrap_or(DEFAULT_WITHIN_HOURS))
                }
                At::Today => {
                    let today = now.with_timezone(&chrono_tz::Europe::London).date_naive();
                    london_midnight(today.succ_opt().unwrap_or(today))
                }
            });
        }
    }

    if near.is_none() && radius_km.is_some() {
        return Err("radius_km requires near".into());
    }
    if near.is_none() && walk_min.is_some() {
        return Err("within_walk_min requires near".into());
    }
    if radius_km.is_some() && walk_min.is_some() {
        return Err("use radius_km or within_walk_min, not both".into());
    }
    let near = near.map(|(lat, lng)| Near {
        lat,
        lng,
        radius_km: walk_min
            .map(walk_radius_km)
            .or(radius_km)
            .unwrap_or(DEFAULT_RADIUS_KM),
    });
    // Without `sort`: best match for a search, else an area means nearest
    // first (as before `sort` existed).
    let requested = sort.unwrap_or(if filter.search.is_some() {
        Sort::Relevance
    } else if near.is_some() {
        Sort::Nearest
    } else {
        Sort::Soonest
    });
    let (applied, fell_back_from) = match (requested, near) {
        (Sort::Nearest, None) => (Sort::Soonest, Some(Sort::Nearest)),
        (Sort::Relevance, _) if filter.search.is_none() => (Sort::Soonest, Some(Sort::Relevance)),
        (s, _) => (s, None),
    };
    let mismatch = || Err("cursor does not match the sort".to_string());
    let order = match (applied, cursor) {
        (Sort::Soonest, None) => EventOrder::ByStart { after: None },
        (Sort::Soonest, Some(Cursor::Start(t, id))) => EventOrder::ByStart {
            after: Some((t, id)),
        },
        (Sort::Nearest, c) => EventOrder::ByDistance {
            near: near.expect("nearest without near falls back above"),
            after: match c {
                None => None,
                Some(Cursor::Distance(d, id)) => Some((d, id)),
                Some(_) => return mismatch(),
            },
        },
        (Sort::Ending, c) => EventOrder::ByEnd {
            now,
            after: match c {
                None => None,
                Some(Cursor::End(t, id)) => Some((t, id)),
                Some(_) => return mismatch(),
            },
        },
        (Sort::Added, c) => EventOrder::ByAdded {
            after: match c {
                None => None,
                Some(Cursor::Added(t, id)) => Some((t, id)),
                Some(_) => return mismatch(),
            },
        },
        (Sort::Surprise, None) => EventOrder::Shuffled {
            seed: london_today(now),
            after: None,
        },
        (Sort::Surprise, Some(Cursor::Shuffle(seed, id))) => EventOrder::Shuffled {
            seed,
            after: Some(id),
        },
        (Sort::Relevance, c) => EventOrder::ByRelevance {
            after: match c {
                None => None,
                Some(Cursor::Relevance(r, id)) => Some((r, id)),
                Some(_) => return mismatch(),
            },
        },
        (Sort::Soonest | Sort::Surprise, Some(_)) => return mismatch(),
    };
    Ok(EventQuery {
        filter,
        near,
        order,
        fell_back_from,
        limit: limit.unwrap_or(DEFAULT_LIMIT),
        facets,
    })
}

/// Source keys are kebab-case ASCII (`serpentine-galleries`).
pub fn is_source_key(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn parse_date(name: &str, value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("{name} must be a date (YYYY-MM-DD)"))
}

fn london_midnight(date: NaiveDate) -> DateTime<Utc> {
    london_to_utc(date.and_time(NaiveTime::MIN))
}

fn parse_near(value: &str) -> Result<(f64, f64), String> {
    let err = || "near must be <lat>,<lng>".to_string();
    let (lat, lng) = value.split_once(',').ok_or_else(err)?;
    let lat: f64 = lat.trim().parse().map_err(|_| err())?;
    let lng: f64 = lng.trim().parse().map_err(|_| err())?;
    if !((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng)) {
        return Err("near is out of range".into());
    }
    Ok((lat, lng))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn dates_are_london_days_and_to_is_inclusive() {
        let q = parse_query("from=2026-10-01&to=2026-10-01").unwrap();
        assert_eq!(
            q.filter.from,
            Some(Utc.with_ymd_and_hms(2026, 9, 30, 23, 0, 0).unwrap())
        );
        assert_eq!(
            q.filter.until,
            Some(Utc.with_ymd_and_hms(2026, 10, 1, 23, 0, 0).unwrap())
        );
        let q = parse_query("from=2026-12-01").unwrap();
        assert_eq!(
            q.filter.from,
            Some(Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap())
        );
        assert_eq!(q.filter.until, None);
    }

    #[test]
    fn defaults() {
        let q = parse_query("").unwrap();
        assert_eq!(q.filter, EventFilter::default());
        assert_eq!(q.order, EventOrder::ByStart { after: None });
        assert_eq!(q.limit, DEFAULT_LIMIT);
        let EventOrder::ByDistance { near, .. } = parse_query("near=51.5,-0.1").unwrap().order
        else {
            panic!("expected distance order");
        };
        assert_eq!(near.radius_km, DEFAULT_RADIUS_KM);
    }

    #[test]
    fn walking_time_becomes_a_radius() {
        let q = parse_query("near=51.5,-0.1&within_walk_min=20").unwrap();
        let near = q.near.unwrap();
        assert_eq!(near.radius_km, walk_radius_km(20));
        assert_eq!(q.order.sort(), Sort::Nearest);
        // map.mjs: minutes = round(km * 1.3 / 5 * 60), so 1.3 km is "≈ 20
        // min walk" and 1.33 km is "≈ 21 min walk".
        assert!(walk_radius_km(20) >= 1.3 && walk_radius_km(20) < 1.33);
        assert!(walk_radius_km(10) < walk_radius_km(30));
    }

    #[test]
    fn categories_repeat_and_dedupe() {
        let q = parse_query("category=talk&category=workshop&category=talk").unwrap();
        assert_eq!(q.filter.categories, [Category::Talk, Category::Workshop]);
    }

    #[test]
    fn sources_repeat_and_dedupe() {
        let q = parse_query("source=barbican&source=design-museum&source=barbican").unwrap();
        assert_eq!(q.filter.sources, ["barbican", "design-museum"]);
    }

    #[test]
    fn tag_filters_repeat_dedupe_and_use_the_vocabularies() {
        let q = parse_query(
            "medium=photography&medium=painting&medium=photography&format=talk&good_for=kids&facets=true",
        )
        .unwrap();
        assert_eq!(q.filter.mediums, ["photography", "painting"]);
        assert_eq!(q.filter.formats, ["talk"]);
        assert_eq!(q.filter.good_for, ["kids"]);
        assert!(q.facets);
        for raw in [
            "medium=pottery",
            "format=party",
            "good_for=everyone",
            "facets=yes",
        ] {
            assert!(parse_query(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn when_and_price_max_parse() {
        for w in When::ALL {
            let q = parse_query(&format!("when={}", w.as_str())).unwrap();
            assert_eq!(q.filter.when, Some(w));
        }
        let q = parse_query("when=evening&when=weekend").unwrap();
        assert_eq!(q.filter.when, Some(When::Weekend));
        let price = |raw: &str| parse_query(raw).unwrap().filter.price_max;
        assert_eq!(price("price_max=10"), Some(Decimal::from(10)));
        assert_eq!(price("price_max=12.5"), Some(Decimal::new(125, 1)));
        assert_eq!(price("price_max=0"), Some(Decimal::ZERO));
    }

    #[test]
    fn picks_parse_with_the_london_date() {
        use chrono::TimeZone;
        // 23:30 UTC on 24 Oct 2026 is already the 25th in London (BST).
        let now = Utc.with_ymd_and_hms(2026, 10, 24, 23, 30, 0).unwrap();
        for p in Pick::ALL {
            let q = parse_query_at(&format!("pick={}", p.as_str()), now).unwrap();
            assert_eq!(
                q.filter.pick,
                Some(PickFilter {
                    pick: p,
                    today: NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
                })
            );
        }
        assert!(parse_query("pick=tonightly").is_err());
        assert!(parse_query("pick=").is_err());
    }

    #[test]
    fn ids_parse_dedupe_and_cap() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let q = parse_query(&format!("ids={a},{b},%20{a}")).unwrap();
        assert_eq!(q.filter.ids, [a, b]);
        let many: Vec<String> = (0..=MAX_IDS).map(|_| Uuid::new_v4().to_string()).collect();
        assert!(parse_query(&format!("ids={}", many[..MAX_IDS].join(","))).is_ok());
        assert!(parse_query(&format!("ids={}", many.join(","))).is_err());
    }

    #[test]
    fn rejects_bad_input() {
        for raw in [
            "from=2026-10-2x",
            "from=2026-10-02&to=2026-10-01",
            "category=concert",
            "free=yes",
            "near=51.5",
            "near=91,0",
            "near=51.5,-0.1&radius_km=0",
            "near=51.5,-0.1&radius_km=101",
            "within_walk_min=20",
            "near=51.5,-0.1&within_walk_min=0",
            "near=51.5,-0.1&within_walk_min=61",
            "near=51.5,-0.1&within_walk_min=2.5",
            "near=51.5,-0.1&within_walk_min=20&radius_km=2",
            "radius_km=3",
            "limit=0",
            "limit=101",
            "cursor=zz",
            "bogus=1",
            "source=",
            "source=Barbican",
            "source=a%20b",
            "ids=",
            "ids=not-a-uuid",
            "when=night",
            "when=",
            "price_max=abc",
            "price_max=-1",
            "price_max=",
            "sort=popular",
            "sort=",
        ] {
            assert!(parse_query(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn cursors_round_trip_and_must_match_the_sort() {
        let id = Uuid::new_v4();
        let t = Utc.with_ymd_and_hms(2026, 10, 1, 18, 30, 0).unwrap()
            + chrono::Duration::microseconds(123_456);
        for c in [
            Cursor::Start(t, id),
            Cursor::Distance(1.234_567_890_123, id),
            Cursor::End(t, id),
            Cursor::Added(t, id),
            Cursor::Shuffle(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(), id),
        ] {
            assert_eq!(Cursor::decode(&c.encode()), Some(c));
        }
        let start = Cursor::Start(t, id).encode();
        let distance = Cursor::Distance(0.5, id).encode();
        assert!(parse_query(&format!("cursor={start}")).is_ok());
        assert!(parse_query(&format!("cursor={distance}")).is_err());
        assert!(parse_query(&format!("near=51.5,-0.1&cursor={distance}")).is_ok());
        assert!(parse_query(&format!("near=51.5,-0.1&cursor={start}")).is_err());
        // nearest without near falls back to soonest, so takes its cursors.
        assert!(parse_query(&format!("sort=nearest&cursor={start}")).is_ok());
        let end = Cursor::End(t, id).encode();
        assert!(parse_query(&format!("sort=ending&cursor={end}")).is_ok());
        assert!(parse_query(&format!("sort=added&cursor={end}")).is_err());
        assert!(parse_query(&format!("cursor={end}")).is_err());
    }

    #[test]
    fn sorts_parse_with_defaults_and_fallback() {
        let now = Utc.with_ymd_and_hms(2026, 10, 10, 23, 30, 0).unwrap();
        let q = |raw: &str| parse_query_at(raw, now).unwrap();
        assert_eq!(q("").order.sort(), Sort::Soonest);
        assert_eq!(q("near=51.5,-0.1").order.sort(), Sort::Nearest);
        assert_eq!(q("near=51.5,-0.1&sort=soonest").order.sort(), Sort::Soonest);
        assert!(q("near=51.5,-0.1&sort=soonest").near.is_some());
        let fell = q("sort=nearest");
        assert_eq!(fell.order.sort(), Sort::Soonest);
        assert_eq!(fell.fell_back_from, Some(Sort::Nearest));
        assert_eq!(q("sort=nearest&near=51.5,-0.1").fell_back_from, None);
        assert_eq!(
            q("sort=ending").order,
            EventOrder::ByEnd { now, after: None }
        );
        // 23:30 UTC on 10 October is 00:30 on the 11th in London.
        assert_eq!(
            q("sort=surprise").order,
            EventOrder::Shuffled {
                seed: NaiveDate::from_ymd_opt(2026, 10, 11).unwrap(),
                after: None
            }
        );
        let seed = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let id = Uuid::new_v4();
        let c = Cursor::Shuffle(seed, id).encode();
        assert_eq!(
            q(&format!("sort=surprise&cursor={c}")).order,
            EventOrder::Shuffled {
                seed,
                after: Some(id)
            }
        );
        for s in Sort::ALL {
            assert_eq!(Sort::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn q_sorts_by_relevance_unless_near() {
        let q = parse_query("q=%20Sámi%20").unwrap();
        assert_eq!(q.filter.search.as_ref().unwrap().text, "Sámi");
        assert_eq!(q.order, EventOrder::ByRelevance { after: None });
        assert_eq!(parse_query("q=%20%20").unwrap().filter.search, None);
        // With an area too (a filter); another sort wins when chosen.
        let q = parse_query("q=print&near=51.5,-0.1").unwrap();
        assert_eq!(q.order, EventOrder::ByRelevance { after: None });
        assert!(q.near.is_some());
        assert_eq!(
            parse_query("q=print&sort=soonest").unwrap().order.sort(),
            Sort::Soonest
        );
        // relevance without q falls back to soonest.
        let q = parse_query("sort=relevance").unwrap();
        assert_eq!(q.order.sort(), Sort::Soonest);
        assert_eq!(q.fell_back_from, Some(Sort::Relevance));
        assert!(parse_query(&format!("q={}", "x".repeat(201))).is_err());
        let id = Uuid::new_v4();
        let rel = Cursor::Relevance(0.0607927, id);
        assert_eq!(Cursor::decode(&rel.encode()), Some(rel));
        let rel = rel.encode();
        let start = Cursor::Start(Utc::now(), id).encode();
        assert!(parse_query(&format!("q=print&cursor={rel}")).is_ok());
        assert!(parse_query(&format!("q=print&cursor={start}")).is_err());
        assert!(parse_query(&format!("cursor={rel}")).is_err());
        assert!(parse_query(&format!("near=51.5,-0.1&cursor={rel}")).is_err());
        assert!(parse_query(&format!("q=print&near=51.5,-0.1&cursor={rel}")).is_ok());
    }

    #[test]
    fn at_now_and_today_set_a_live_window() {
        // 22:30 BST on Sat 3 Oct 2026 (21:30 UTC).
        let now = Utc.with_ymd_and_hms(2026, 10, 3, 21, 30, 0).unwrap();
        let q = parse_query_at("at=now", now).unwrap();
        assert_eq!(q.filter.live_at, Some(now));
        assert_eq!(
            q.filter.from,
            Some(now - chrono::Duration::hours(LIVE_LOOKBACK_HOURS))
        );
        assert_eq!(q.filter.until, Some(now + chrono::Duration::hours(3)));
        let q = parse_query_at("at=now&within_hours=1&near=51.5,-0.1", now).unwrap();
        assert_eq!(q.filter.until, Some(now + chrono::Duration::hours(1)));
        assert!(matches!(q.order, EventOrder::ByDistance { .. }));
        // "Today" ends at London midnight (23:00 UTC while on BST).
        let q = parse_query_at("at=today", now).unwrap();
        assert_eq!(
            q.filter.until,
            Some(Utc.with_ymd_and_hms(2026, 10, 3, 23, 0, 0).unwrap())
        );
        // After midnight UTC but before London midnight on a GMT day: still that London day.
        let late = Utc.with_ymd_and_hms(2026, 12, 1, 23, 30, 0).unwrap();
        let q = parse_query_at("at=today", late).unwrap();
        assert_eq!(
            q.filter.until,
            Some(Utc.with_ymd_and_hms(2026, 12, 2, 0, 0, 0).unwrap())
        );
        assert_eq!(parse_query_at("", now).unwrap().filter.live_at, None);
    }

    #[test]
    fn at_rejects_bad_combinations() {
        let now = Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap();
        for raw in [
            "at=later",
            "at=",
            "at=now&from=2026-10-03",
            "at=now&to=2026-10-03",
            "at=today&when=evening",
            "at=today&within_hours=2",
            "within_hours=3",
            "at=now&within_hours=0",
            "at=now&within_hours=25",
            "at=now&within_hours=two",
        ] {
            assert!(parse_query_at(raw, now).is_err(), "{raw}");
        }
    }

    #[test]
    fn bounding_box_contains_the_radius() {
        let near = Near {
            lat: 51.5,
            lng: -0.1,
            radius_km: 5.0,
        };
        let b = near.bounding_box();
        // 5 km is ~0.045 degrees of latitude and ~0.072 of longitude here.
        assert!((b.max_lat - 51.5 - 0.0449).abs() < 0.001, "{b:?}");
        assert!((b.max_lng + 0.1 - 0.0722).abs() < 0.001, "{b:?}");
        assert!(b.min_lat < 51.5 && b.min_lng < -0.1);
    }
}
