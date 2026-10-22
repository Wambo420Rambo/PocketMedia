//! Download from Twitter, the parts without any network or disk: reading a
//! post link, the request for a post's details, and the pictures, videos and
//! GIFs those details list. `download.rs` does the fetching on a worker thread.
//!
//! A post's details come from the endpoint Twitter's own embedded posts use
//! (`cdn.syndication.twimg.com/tweet-result`): no account or key is needed, but it
//! is not a documented API, so a change on Twitter's side can stop it working.

use serde_json::Value;

pub const API_HOST: &str = "cdn.syndication.twimg.com";

/// The most links one download takes.
pub const MAX_LINKS: usize = 20;

/// A video larger than this is not downloaded.
pub const MAX_VIDEO_BYTES: u64 = 1024 * 1024 * 1024;
/// Nor a picture larger than this.
pub const MAX_PHOTO_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaType {
    /// A picture, saved as .jpg or .png at its original size.
    Photo,
    Video,
    /// Twitter keeps "GIFs" as short silent MP4 videos; PocketMedia turns them
    /// back into .gif files.
    Gif,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Media {
    pub kind: MediaType,
    /// What to download: the original picture, or the highest-quality MP4.
    pub url: String,
    /// The downloaded file's extension: "jpg", "png" or "mp4".
    pub ext: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Post {
    pub id: u64,
    /// The account name without the @ (empty when the details leave it out).
    pub user: String,
    pub media: Vec<Media>,
}

/// The post number in a post link: `https://twitter.com/name/status/123…`,
/// `x.com/…`, `mobile.twitter.com/…`, `…/i/status/123`, or `…/statuses/123`,
/// with or without `https://`, `www.` and anything after the number.
/// None: not a post link.
pub fn post_id(link: &str) -> Option<u64> {
    let s = link.trim();
    let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let (host, path) = s.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").or_else(|| host.strip_prefix("mobile.")).unwrap_or(&host);
    if !matches!(host, "twitter.com" | "x.com") {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let at = parts.iter().position(|p| *p == "status" || *p == "statuses")?;
    if at != 1 {
        return None;
    }
    let id = parts.get(at + 1)?;
    if id.is_empty() || id.len() > 20 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    id.parse().ok().filter(|n| *n > 0)
}

/// Splits pasted text into one entry per link (spaces and line breaks separate them).
pub fn split_links(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

/// The path (with query) that asks for post `id`'s details.
pub fn details_path(id: u64) -> String {
    format!("/tweet-result?id={id}&lang=en&token={}", token(id))
}

/// The `token` the endpoint expects with a post number: (id / 10^15) * π written
/// in base 36 the way JavaScript's `toString(36)` writes it, without its zeros
/// and its point.
pub fn token(id: u64) -> String {
    radix36((id as f64 / 1e15) * std::f64::consts::PI).chars().filter(|c| *c != '0' && *c != '.').collect()
}

/// A positive number in base 36, digit for digit what V8 (Chrome's and Node's
/// JavaScript engine) gives for `Number.prototype.toString(36)`: fraction digits
/// only as far as the number's precision, rounded the same way.
fn radix36(value: f64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    const RADIX: f64 = 36.0;
    let mut integer = value.floor();
    let mut fraction = value - integer;
    // Half the distance to the next number a double can hold.
    let mut delta = (0.5 * (f64::from_bits(value.to_bits() + 1) - value)).max(f64::from_bits(1));
    let mut frac_digits: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= RADIX;
            delta *= RADIX;
            let digit = fraction as usize;
            frac_digits.push(DIGITS[digit]);
            fraction -= digit as f64;
            if (fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1)) && fraction + delta > 1.0 {
                // Round up, carrying into earlier digits (and into the whole part).
                loop {
                    match frac_digits.pop() {
                        None => {
                            integer += 1.0;
                            break;
                        }
                        Some(c) => {
                            let d = DIGITS.iter().position(|x| *x == c).unwrap_or(0);
                            if d + 1 < 36 {
                                frac_digits.push(DIGITS[d + 1]);
                                break;
                            }
                        }
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    let mut int_digits = Vec::new();
    loop {
        let remainder = integer % RADIX;
        int_digits.push(DIGITS[remainder as usize]);
        integer = (integer - remainder) / RADIX;
        if integer <= 0.0 {
            break;
        }
    }
    int_digits.reverse();
    let mut s = String::from_utf8(int_digits).unwrap_or_default();
    if !frac_digits.is_empty() {
        s.push('.');
        s.push_str(std::str::from_utf8(&frac_digits).unwrap_or(""));
    }
    s
}

/// Reads the endpoint's answer for post `id`. Err: a sentence saying why there
/// is nothing to download.
pub fn parse_post(id: u64, body: &[u8]) -> Result<Post, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "Twitter sent an answer PocketMedia can't read.".to_string())?;
    let obj = v.as_object().ok_or("Twitter sent an answer PocketMedia can't read.")?;
    if obj.is_empty() {
        return Err(NOT_FOUND.into());
    }
    if obj.get("__typename").and_then(Value::as_str) == Some("TweetTombstone") {
        return Err("This post is not available (deleted, private or age-restricted).".into());
    }
    let user = obj.get("user").and_then(|u| u.get("screen_name")).and_then(Value::as_str).unwrap_or("").to_string();
    let media: Vec<Media> = obj
        .get("mediaDetails")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(media_of).collect())
        .unwrap_or_default();
    if media.is_empty() {
        return Err(NOTHING.into());
    }
    Ok(Post { id, user, media })
}

pub const NOTHING: &str = "This post has no pictures, videos or GIFs.";
pub const NOT_FOUND: &str = "Post not found. Check the link; deleted and private posts can't be downloaded.";

/// One entry of `mediaDetails`: a picture at its original size, or a video or
/// GIF and its best MP4.
fn media_of(m: &Value) -> Option<Media> {
    let kind = match m.get("type").and_then(Value::as_str)? {
        "photo" => return photo_of(m),
        "video" => MediaType::Video,
        "animated_gif" => MediaType::Gif,
        _ => return None,
    };
    let variants = m.get("video_info")?.get("variants")?.as_array()?;
    let best = variants
        .iter()
        .filter(|v| v.get("content_type").and_then(Value::as_str) == Some("video/mp4"))
        .filter_map(|v| Some((v.get("bitrate").and_then(Value::as_u64).unwrap_or(0), v.get("url")?.as_str()?)))
        .filter(|(_, url)| url.starts_with("https://"))
        .max_by_key(|(bitrate, _)| *bitrate)?;
    Some(Media { kind, url: best.1.to_string(), ext: "mp4" })
}

/// A picture: `https://pbs.twimg.com/media/<id>.<ext>` asked for at its original
/// size (`name=orig`). A PNG stays a PNG; anything else (JPG, WebP) comes as JPG,
/// a format the library takes.
fn photo_of(m: &Value) -> Option<Media> {
    let url = m.get("media_url_https")?.as_str()?;
    let url = url.split(['?', '#']).next()?;
    if !url.starts_with("https://") {
        return None;
    }
    let (base, ext) = match url.rsplit_once('.') {
        Some((b, e)) if !e.contains('/') && !b.ends_with('/') => (b, e.to_ascii_lowercase()),
        _ => (url, String::new()),
    };
    let ext = if ext == "png" { "png" } else { "jpg" };
    Some(Media { kind: MediaType::Photo, url: format!("{base}?format={ext}&name=orig"), ext })
}

/// The file name (without extension) for item `index` of `count` in a post:
/// `name_1234567890`, or `name_1234567890_2` when the post has more than one.
/// Characters Windows does not allow in names are left out.
pub fn file_base(post: &Post, index: usize, count: usize) -> String {
    let user: String = post.user.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    let base = if user.is_empty() { format!("twitter_{}", post.id) } else { format!("{user}_{}", post.id) };
    if count > 1 { format!("{base}_{}", index + 1) } else { base }
}

/// Splits `https://host/path?query` into host and path. None for anything else.
pub fn split_https(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let slash = rest.find('/').unwrap_or(rest.len());
    let (host, path) = rest.split_at(slash);
    if host.is_empty() || host.contains(['@', ':']) {
        return None;
    }
    Some((host, if path.is_empty() { "/" } else { path }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_links_in_every_usual_form() {
        for link in [
            "https://twitter.com/name/status/1577730467436138524",
            "https://x.com/name/status/1577730467436138524?s=20&t=abc",
            "http://www.twitter.com/name/status/1577730467436138524/",
            "mobile.twitter.com/name/status/1577730467436138524/photo/1",
            "  https://X.com/i/status/1577730467436138524#m  ",
            "twitter.com/name/statuses/1577730467436138524",
            "https://x.com/name/status/1577730467436138524/video/1",
        ] {
            assert_eq!(post_id(link), Some(1577730467436138524), "{link}");
        }
        for link in [
            "",
            "https://twitter.com/name",
            "https://example.com/name/status/123",
            "https://nottwitter.com/name/status/123",
            "https://twitter.com/name/status/abc",
            "https://twitter.com/status/123",
            "https://twitter.com/name/likes/status/123",
            "https://twitter.com/name/status/",
            "https://twitter.com/name/status/0",
            "https://twitter.com/name/status/123456789012345678901234",
        ] {
            assert_eq!(post_id(link), None, "{link}");
        }
    }

    #[test]
    fn pasted_text_becomes_one_entry_per_link() {
        assert_eq!(split_links("a\r\nb  c\td\n"), vec!["a", "b", "c", "d"]);
        assert!(split_links(" \n ").is_empty());
    }

    /// Values from Node (V8): `((id / 1e15) * Math.PI).toString(36).replace(/(0+|\.)/g, '')`.
    #[test]
    fn the_token_matches_javascript() {
        for (id, expected) in TOKENS {
            assert_eq!(token(*id), *expected, "{id}");
        }
    }

    const TOKENS: &[(u64, &str)] = &[
        (20, "6dq1a2xwd93"),
        (1577730467436138524, "3tol417ti8o"),
        (1, "bhi2ay3f28n"),
        (999, "8uqo9xs4uzm"),
        (1234567890123456789, "2zqic77uqyk"),
        (1849000000000000000, "4hcsz1indrr"),
        (1460323737035677698, "3jfqq1vhqna"),
        (463440424141459456, "14fxvks611f"),
        (1700000000000000001, "44cpgxmyurn"),
        (18446744073709551615, "18ps5lqos4lc"),
        (1000000000000000, "353i5ab8p5f"),
        (1720000000000000000, "463jfkp3ln"),
        (1580661436132757506, "3txslhgbjje"),
        (1629307668568633344, "3y6mctgwzxo"),
        (1852471046102859979, "4hnpjf5fzj"),
    ];

    #[test]
    fn original_pictures_and_the_best_mp4_of_each_video_and_gif() {
        let body = br#"{
            "__typename": "Tweet", "id_str": "20", "user": {"screen_name": "jack"},
            "mediaDetails": [
                {"type": "photo", "media_url_https": "https://pbs.twimg.com/media/Abc_1.jpg"},
                {"type": "photo", "media_url_https": "https://pbs.twimg.com/media/Def-2.png"},
                {"type": "photo", "media_url_https": "https://pbs.twimg.com/media/Ghi.webp"},
                {"type": "video", "video_info": {"variants": [
                    {"content_type": "application/x-mpegURL", "url": "https://video.twimg.com/a.m3u8"},
                    {"bitrate": 256000, "content_type": "video/mp4", "url": "https://video.twimg.com/low.mp4"},
                    {"bitrate": 2176000, "content_type": "video/mp4", "url": "https://video.twimg.com/high.mp4"},
                    {"bitrate": 832000, "content_type": "video/mp4", "url": "https://video.twimg.com/mid.mp4"}
                ]}},
                {"type": "animated_gif", "video_info": {"variants": [
                    {"bitrate": 0, "content_type": "video/mp4", "url": "https://video.twimg.com/tweet_video/g.mp4"}
                ]}}
            ]
        }"#;
        let post = parse_post(20, body).unwrap();
        assert_eq!(post.user, "jack");
        assert_eq!(
            post.media,
            vec![
                Media { kind: MediaType::Photo, url: "https://pbs.twimg.com/media/Abc_1?format=jpg&name=orig".into(), ext: "jpg" },
                Media { kind: MediaType::Photo, url: "https://pbs.twimg.com/media/Def-2?format=png&name=orig".into(), ext: "png" },
                Media { kind: MediaType::Photo, url: "https://pbs.twimg.com/media/Ghi?format=jpg&name=orig".into(), ext: "jpg" },
                Media { kind: MediaType::Video, url: "https://video.twimg.com/high.mp4".into(), ext: "mp4" },
                Media { kind: MediaType::Gif, url: "https://video.twimg.com/tweet_video/g.mp4".into(), ext: "mp4" },
            ]
        );
        assert_eq!(file_base(&post, 0, 5), "jack_20_1");
        assert_eq!(file_base(&post, 4, 5), "jack_20_5");
    }

    #[test]
    fn posts_without_anything_to_download_say_why() {
        assert_eq!(parse_post(1, b"{}").unwrap_err(), NOT_FOUND);
        assert!(parse_post(1, br#"{"__typename":"TweetTombstone"}"#).unwrap_err().contains("not available"));
        assert_eq!(parse_post(1, br#"{"__typename":"Tweet","text":"hi"}"#).unwrap_err(), NOTHING);
        let photo = br#"{"mediaDetails":[{"type":"photo","media_url_https":"https://pbs.twimg.com/media/a.jpg?x=1"}]}"#;
        assert_eq!(parse_post(1, photo).unwrap().media[0].url, "https://pbs.twimg.com/media/a?format=jpg&name=orig");
        let plain_photo = br#"{"mediaDetails":[{"type":"photo","media_url_https":"http://pbs.twimg.com/media/a.jpg"}]}"#;
        assert_eq!(parse_post(1, plain_photo).unwrap_err(), NOTHING);
        assert!(parse_post(1, b"<html>").unwrap_err().contains("can't read"));
        // An MP4 that is not https is not used.
        let plain = br#"{"mediaDetails":[{"type":"video","video_info":{"variants":[{"content_type":"video/mp4","url":"http://x/a.mp4"}]}}]}"#;
        assert!(parse_post(1, plain).is_err());
    }

    #[test]
    fn names_keep_only_safe_characters() {
        let post = Post { id: 7, user: "a.b/c<d>_e".into(), media: vec![] };
        assert_eq!(file_base(&post, 0, 1), "abcd_e_7");
        let nobody = Post { id: 7, user: String::new(), media: vec![] };
        assert_eq!(file_base(&nobody, 0, 1), "twitter_7");
    }

    #[test]
    fn https_urls_split_into_host_and_path() {
        assert_eq!(split_https("https://video.twimg.com/a/b.mp4?tag=12"), Some(("video.twimg.com", "/a/b.mp4?tag=12")));
        assert_eq!(split_https("https://host"), Some(("host", "/")));
        assert_eq!(split_https("http://host/a"), None);
        assert_eq!(split_https("https://user@host/a"), None);
        assert_eq!(split_https("https://host:8080/a"), None);
    }
}
