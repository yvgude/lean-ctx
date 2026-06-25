use std::sync::Arc;

use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderValue;

#[derive(Debug, Default)]
struct ChatGptCloudflareCookieStore {
    jar: Jar,
}

impl CookieStore for ChatGptCloudflareCookieStore {
    fn set_cookies(
        &self,
        cookie_headers: &mut dyn Iterator<Item = &HeaderValue>,
        url: &reqwest::Url,
    ) {
        if !is_chatgpt_cookie_url(url) {
            return;
        }

        let mut cloudflare_cookie_headers =
            cookie_headers.filter(|header| is_allowed_cloudflare_set_cookie_header(header));
        self.jar.set_cookies(&mut cloudflare_cookie_headers, url);
    }

    fn cookies(&self, url: &reqwest::Url) -> Option<HeaderValue> {
        if is_chatgpt_cookie_url(url) {
            self.jar
                .cookies(url)
                .and_then(|cookies| only_cloudflare_cookies(&cookies))
        } else {
            None
        }
    }
}

pub(super) fn with_chatgpt_cloudflare_cookie_store(
    builder: reqwest::ClientBuilder,
) -> reqwest::ClientBuilder {
    builder.cookie_provider(Arc::new(ChatGptCloudflareCookieStore::default()))
}

fn is_chatgpt_cookie_url(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    is_allowed_chatgpt_host(host)
}

fn is_allowed_chatgpt_host(host: &str) -> bool {
    const EXACT_HOSTS: &[&str] = &["chatgpt.com", "chat.openai.com", "chatgpt-staging.com"];
    const SUBDOMAIN_SUFFIXES: &[&str] = &[".chatgpt.com", ".chatgpt-staging.com"];

    EXACT_HOSTS.contains(&host)
        || SUBDOMAIN_SUFFIXES
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

fn is_allowed_cloudflare_set_cookie_header(header: &HeaderValue) -> bool {
    header
        .to_str()
        .ok()
        .and_then(set_cookie_name)
        .is_some_and(is_allowed_cloudflare_cookie_name)
}

fn set_cookie_name(header: &str) -> Option<&str> {
    let (name, _) = header.split_once('=')?;
    let name = name.trim();
    (!name.is_empty()).then_some(name)
}

fn only_cloudflare_cookies(header: &HeaderValue) -> Option<HeaderValue> {
    let header = header.to_str().ok()?;
    let cookies = header
        .split(';')
        .filter_map(|cookie| {
            let cookie = cookie.trim();
            let name = cookie.split_once('=')?.0.trim();
            is_allowed_cloudflare_cookie_name(name).then_some(cookie)
        })
        .collect::<Vec<_>>()
        .join("; ");

    if cookies.is_empty() {
        None
    } else {
        HeaderValue::from_str(&cookies).ok()
    }
}

fn is_allowed_cloudflare_cookie_name(name: &str) -> bool {
    matches!(
        name,
        "__cf_bm"
            | "__cflb"
            | "__cfruid"
            | "__cfseq"
            | "__cfwaitingroom"
            | "_cfuvid"
            | "cf_clearance"
            | "cf_ob_info"
            | "cf_use_ob"
    ) || name.starts_with("cf_chl_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_only_cloudflare_cookies_for_chatgpt_hosts() {
        let store = ChatGptCloudflareCookieStore::default();
        let url = reqwest::Url::parse("https://chatgpt.com/backend-api/ps/mcp").unwrap();
        let cf = HeaderValue::from_static("cf_clearance=ok; Path=/; Secure; HttpOnly");
        let account = HeaderValue::from_static("__Secure-next-auth.session-token=secret; Path=/");

        store.set_cookies(&mut [&cf, &account].into_iter(), &url);

        let cookies = store.cookies(&url).unwrap();
        assert_eq!(cookies.to_str().unwrap(), "cf_clearance=ok");
    }

    #[test]
    fn rejects_non_chatgpt_cookie_urls() {
        let store = ChatGptCloudflareCookieStore::default();
        let url = reqwest::Url::parse("https://api.openai.com/v1/responses").unwrap();
        let cf = HeaderValue::from_static("cf_clearance=ok; Path=/; Secure; HttpOnly");

        store.set_cookies(&mut std::iter::once(&cf), &url);

        assert!(store.cookies(&url).is_none());
    }

    #[test]
    fn rejects_plain_http_chatgpt_cookie_urls() {
        let store = ChatGptCloudflareCookieStore::default();
        let http_url = reqwest::Url::parse("http://chatgpt.com/backend-api/ps/mcp").unwrap();
        let https_url = reqwest::Url::parse("https://chatgpt.com/backend-api/ps/mcp").unwrap();
        let cf = HeaderValue::from_static("cf_clearance=ok; Path=/; Secure; HttpOnly");

        store.set_cookies(&mut std::iter::once(&cf), &http_url);

        assert!(store.cookies(&https_url).is_none());
    }
}
