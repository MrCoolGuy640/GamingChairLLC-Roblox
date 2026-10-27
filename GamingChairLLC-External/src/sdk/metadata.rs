//! Roblox web metadata: users, avatar thumbnails and place/game details.
//!
//! Everything here is read-only public API from `users.roblox.com`,
//! `thumbnails.roblox.com`, `games.roblox.com` and `apis.roblox.com`. The
//! helpers are groundwork for the UI (player card, current-game page) and are
//! not wired into any view yet.
//!
//! Thumbnail URLs are plain PNGs. Passing `is_circular = true` asks Roblox for
//! a round crop; either way the image keeps a transparent background.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};

const TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = "GamingChairLLC/0.1 (external tool)";
/// Avatar images are always requested at this square size, like the website.
const AVATAR_SIZE: &str = "420x420";

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInfo {
    pub user_id: i64,
    pub username: String,
    pub display_name: String,
    /// Full-body avatar PNG. `None` while Roblox is still rendering it.
    pub avatar_url: Option<String>,
    /// Headshot PNG. `None` while Roblox is still rendering it.
    pub headshot_url: Option<String>,
}

impl UserInfo {
    pub fn to_json(&self) -> Value {
        json!({
            "user_id": self.user_id,
            "username": self.username,
            "display_name": self.display_name,
            "avatar_url": self.avatar_url,
            "headshot_url": self.headshot_url,
        })
    }
}

/// Public profile + avatar thumbnails for `user_id`.
///
/// `is_circular` is forwarded to the thumbnail endpoints (`isCircular=true`
/// gives the round crop used on the Roblox site).
pub fn get_user(user_id: i64, is_circular: bool) -> Result<UserInfo> {
    if user_id <= 0 {
        bail!("user_id must be a positive integer");
    }

    let client = client()?;
    let user = get_json(
        &client,
        &format!("https://users.roblox.com/v1/users/{user_id}"),
        &[],
    )?;

    let id = user
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Roblox user {user_id} was not found"))?;

    let username = text(&user, "name");
    let mut display_name = text(&user, "displayName");
    if display_name.is_empty() {
        display_name = username.clone();
    }

    Ok(UserInfo {
        user_id: id,
        username,
        display_name,
        avatar_url: user_thumbnail(&client, "avatar", user_id, is_circular),
        headshot_url: user_thumbnail(&client, "avatar-headshot", user_id, is_circular),
    })
}

fn user_thumbnail(client: &Client, kind: &str, user_id: i64, is_circular: bool) -> Option<String> {
    let value = get_json(
        client,
        &format!("https://thumbnails.roblox.com/v1/users/{kind}"),
        &[
            ("userIds", user_id.to_string()),
            ("size", AVATAR_SIZE.to_string()),
            ("format", "Png".to_string()),
            ("isCircular", bool_param(is_circular).to_string()),
        ],
    )
    .ok()?;

    let entry = first(&value)?;
    if entry.get("state").and_then(Value::as_str) != Some("Completed") {
        return None;
    }

    entry
        .get("imageUrl")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Places / games
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaceInfo {
    pub place_id: i64,
    pub universe_id: i64,
    pub name: String,
    pub description: String,
    pub creator_id: Option<i64>,
    pub creator_name: String,
    pub creator_type: String,
    /// Players currently in the game.
    pub playing: i64,
    pub visits: i64,
    pub favorites: i64,
    pub max_players: i64,
    pub created: String,
    pub updated: String,
    pub likes: i64,
    pub dislikes: i64,
    pub icon_url: String,
    pub thumbnail_urls: Vec<String>,
}

impl PlaceInfo {
    pub fn to_json(&self) -> Value {
        json!({
            "place_id": self.place_id,
            "universe_id": self.universe_id,
            "name": self.name,
            "description": self.description,
            "creator_id": self.creator_id,
            "creator_name": self.creator_name,
            "creator_type": self.creator_type,
            "playing": self.playing,
            "visits": self.visits,
            "favorites": self.favorites,
            "max_players": self.max_players,
            "created": self.created,
            "updated": self.updated,
            "likes": self.likes,
            "dislikes": self.dislikes,
            "icon_url": self.icon_url,
            "thumbnail_urls": self.thumbnail_urls,
        })
    }
}

/// `PlaceId` -> `UniverseId`. `Ok(None)` when Roblox does not know the place.
pub fn get_universe_id(place_id: i64) -> Result<Option<i64>> {
    if place_id <= 0 {
        bail!("place_id must be a positive integer");
    }

    let client = client()?;
    let url = format!("https://apis.roblox.com/universes/v1/places/{place_id}/universe");
    let response = client
        .get(&url)
        .send()
        .with_context(|| format!("requesting {url}"))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    let response = response
        .error_for_status()
        .with_context(|| format!("{url} returned an error"))?;
    let value: Value = response
        .json()
        .with_context(|| format!("decoding the JSON from {url}"))?;

    Ok(value.get("universeId").and_then(Value::as_i64))
}

/// Full web metadata for a place: name, creator, player counts, votes, icon and
/// up to five 768x432 thumbnails (skipped when `thumbnails` is false).
pub fn get_place_info(place_id: i64, thumbnails: bool) -> Result<PlaceInfo> {
    let universe_id = get_universe_id(place_id)?
        .ok_or_else(|| anyhow!("PlaceId {place_id} was not found"))?;

    let client = client()?;
    let mut info = PlaceInfo {
        place_id,
        universe_id,
        ..PlaceInfo::default()
    };

    let universe = universe_id.to_string();

    // Details. A failure here is not fatal: the caller still gets place and
    // universe ids so it can retry later.
    if let Ok(value) = get_json(
        &client,
        "https://games.roblox.com/v1/games",
        &[("universeIds", universe.clone())],
    ) {
        if let Some(game) = first(&value) {
            info.name = text(game, "name");
            info.description = text(game, "description");

            let creator = game.get("creator").unwrap_or(&Value::Null);
            info.creator_id = creator.get("id").and_then(Value::as_i64);
            info.creator_name = text(creator, "name");
            info.creator_type = text(creator, "type");

            info.playing = int(game, "playing");
            info.visits = int(game, "visits");
            info.favorites = int(game, "favoritedCount");
            info.max_players = int(game, "maxPlayers");
            info.created = text(game, "created");
            info.updated = text(game, "updated");
        }
    }

    if let Ok(value) = get_json(
        &client,
        "https://games.roblox.com/v1/games/votes",
        &[("universeIds", universe.clone())],
    ) {
        if let Some(votes) = first(&value) {
            info.likes = int(votes, "upVotes");
            info.dislikes = int(votes, "downVotes");
        }
    }

    if let Ok(value) = get_json(
        &client,
        "https://thumbnails.roblox.com/v1/games/icons",
        &[
            ("universeIds", universe.clone()),
            ("size", "512x512".to_string()),
            ("format", "Png".to_string()),
            ("isCircular", bool_param(false).to_string()),
        ],
    ) {
        if let Some(icon) = first(&value) {
            info.icon_url = text(icon, "imageUrl");
        }
    }

    if thumbnails {
        if let Ok(value) = get_json(
            &client,
            "https://thumbnails.roblox.com/v1/games/multiget/thumbnails",
            &[
                ("universeIds", universe),
                ("size", "768x432".to_string()),
                ("format", "Png".to_string()),
                ("countPerUniverse", "5".to_string()),
            ],
        ) {
            if let Some(entry) = first(&value) {
                info.thumbnail_urls = entry
                    .get("thumbnails")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|row| row.get("imageUrl"))
                            .filter_map(Value::as_str)
                            .filter(|url| !url.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
    }

    Ok(info)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn client() -> Result<Client> {
    Client::builder()
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .context("building the Roblox web client")
}

fn get_json(client: &Client, url: &str, query: &[(&str, String)]) -> Result<Value> {
    let response = client
        .get(url)
        .query(query)
        .send()
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()
        .with_context(|| format!("{url} returned an error"))?;

    response
        .json()
        .with_context(|| format!("decoding the JSON from {url}"))
}

fn bool_param(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn first(value: &Value) -> Option<&Value> {
    value.get("data")?.as_array()?.first()
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn int(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{first, int, text};
    use serde_json::json;

    #[test]
    fn reads_the_first_data_row() {
        let value = json!({ "data": [{ "imageUrl": "a.png" }, { "imageUrl": "b.png" }] });
        assert_eq!(
            first(&value).and_then(|row| row.get("imageUrl")).and_then(|url| url.as_str()),
            Some("a.png")
        );

        let empty = json!({ "data": [] });
        assert!(first(&empty).is_none());
    }

    #[test]
    fn missing_fields_fall_back_to_empty_values() {
        let value = json!({ "name": "Gaming Chair", "playing": 42 });

        assert_eq!(text(&value, "name"), "Gaming Chair");
        assert_eq!(text(&value, "description"), "");
        assert_eq!(int(&value, "playing"), 42);
        assert_eq!(int(&value, "visits"), 0);
    }

    #[test]
    #[ignore = "network"]
    fn live_user_lookup() {
        let user = super::get_user(1, true).unwrap();
        assert_eq!(user.user_id, 1);
        assert!(!user.username.is_empty());
        assert!(user.avatar_url.is_some(), "avatar url missing");
        assert!(user.headshot_url.is_some(), "headshot url missing");
    }

    #[test]
    #[ignore = "network"]
    fn live_place_lookup() {
        let place = super::get_place_info(1818, true).unwrap();
        assert_eq!(place.place_id, 1818);
        assert!(place.universe_id > 0);
        assert!(!place.name.is_empty());
        assert!(!place.icon_url.is_empty());
    }
}
