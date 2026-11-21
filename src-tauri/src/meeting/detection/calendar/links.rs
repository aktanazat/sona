use url::Url;

const MAX_REDIRECTS: usize = 4;

const HTTPS_SCHEME: &[u8] = b"https://";

pub(super) fn calendar_join_url(url: Option<String>, text: Option<&str>) -> Option<String> {
    url.or_else(|| {
        text?
            .split(|character: char| {
                character.is_whitespace() || matches!(character, '<' | '>' | '"' | '\'')
            })
            .find_map(|token| {
                let start = token
                    .as_bytes()
                    .windows(HTTPS_SCHEME.len())
                    .position(|scheme| scheme.eq_ignore_ascii_case(HTTPS_SCHEME))?;
                let candidate = &token[start..];
                let closing = match token[..start].chars().next_back() {
                    Some('(') => Some(')'),
                    Some('[') => Some(']'),
                    _ => None,
                };
                let candidate = closing
                    .and_then(|closing| candidate.rfind(closing))
                    .map_or(candidate, |end| &candidate[..end]);
                is_meeting_link(candidate).then(|| candidate.to_owned())
            })
    })
}

fn is_meeting_link(value: &str) -> bool {
    let Ok(mut url) = Url::parse(value) else {
        return false;
    };
    for depth in 0..=MAX_REDIRECTS {
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        let path = url.path();
        let meeting = match host {
            "meet.google.com" => {
                path.strip_prefix("/lookup/")
                    .is_some_and(|name| !name.is_empty())
                    || path.strip_prefix('/').is_some_and(|code| {
                        code.len() == 12
                            && code.bytes().enumerate().all(|(index, byte)| match index {
                                3 | 8 => byte == b'-',
                                _ => byte.is_ascii_alphabetic(),
                            })
                    })
            }
            "teams.microsoft.com" | "teams.live.com" => {
                path.starts_with("/l/meetup-join/") || path.starts_with("/meet/")
            }
            _ if host == "zoom.us" || host.ends_with(".zoom.us") => {
                path.starts_with("/j/")
                    || path.starts_with("/s/")
                    || path.starts_with("/my/")
                    || path.starts_with("/wc/join/")
            }
            _ if host.ends_with(".webex.com") => {
                path.starts_with("/meet/")
                    || path.starts_with("/join/")
                    || path.ends_with("/j.php")
                    || path.ends_with("/m.php")
            }
            _ => false,
        };
        if meeting {
            return true;
        }
        if depth == MAX_REDIRECTS {
            return false;
        }
        let keys: &[&str] = if host == "safelinks.protection.outlook.com"
            || host.ends_with(".safelinks.protection.outlook.com")
        {
            &["url"]
        } else if matches!(host, "google.com" | "www.google.com") && path == "/url" {
            &["q", "url"]
        } else {
            return false;
        };
        let Some(target) = url
            .query_pairs()
            .find_map(|(key, value)| keys.contains(&key.as_ref()).then_some(value))
        else {
            return false;
        };
        let Ok(next) = Url::parse(&target) else {
            return false;
        };
        url = next;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::calendar_join_url;

    #[test]
    fn a_safelinks_location_keeps_the_protected_join_url() {
        let link = "https://nam12.safelinks.protection.outlook.com/?url=https%3A%2F%2Fus02web.zoom.us%2Fj%2F123456789%3Fpwd%3DA%252Bb%253D&data=fixture&sdata=fixture&reserved=0";

        assert_eq!(calendar_join_url(None, Some(link)).as_deref(), Some(link));
    }

    #[test]
    fn a_google_redirect_in_notes_keeps_the_original_join_url() {
        let text = "Agenda. Join: (https://www.google.com/url?q=https%3A%2F%2Fmeet.google.com%2Fabc-defg-hij&sa=D).";

        assert_eq!(
            calendar_join_url(None, Some(text)).as_deref(),
            Some("https://www.google.com/url?q=https%3A%2F%2Fmeet.google.com%2Fabc-defg-hij&sa=D")
        );
    }

    #[test]
    fn nested_calendar_redirects_keep_the_outer_protected_url() {
        let link = "HTTPS://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fwww.google.com%2Furl%3Fq%3Dhttps%253A%252F%252Fteams.microsoft.com%252Fl%252Fmeetup-join%252Ffixture%253Fcontext%253D%25257B%252522Tid%252522%25253A%252522fixture%252522%25257D";

        assert_eq!(calendar_join_url(None, Some(link)).as_deref(), Some(link));
    }

    #[test]
    fn an_explicit_event_url_is_not_replaced_by_a_link_in_its_details() {
        assert_eq!(
            calendar_join_url(
                Some("https://calendar.example.com/event/123".to_string()),
                Some("https://meet.google.com/abc-defg-hij"),
            )
            .as_deref(),
            Some("https://calendar.example.com/event/123")
        );
    }

    #[test]
    fn a_direct_call_in_notes_keeps_its_encoded_passcode() {
        let text = "Встреча:https://us02web.zoom.us/j/123456789?pwd=A%2Bb%2Fz%3D&uname=Guest%20User#success";

        assert_eq!(
            calendar_join_url(None, Some(text)).as_deref(),
            Some("https://us02web.zoom.us/j/123456789?pwd=A%2Bb%2Fz%3D&uname=Guest%20User#success")
        );
    }

    #[test]
    fn unrelated_calendar_links_do_not_become_join_links() {
        let text = "Agenda: https://zoom.us/pricing https://meet.google.com/ https://meet.google.com.attacker.example/abc-defg-hij https://attacker.example/?url=https%3A%2F%2Fmeet.google.com%2Fabc-defg-hij https://safelinks.protection.outlook.com.attacker.example/?url=https%3A%2F%2Fmeet.google.com%2Fabc-defg-hij";

        assert_eq!(calendar_join_url(None, Some(text)), None);
    }

    #[test]
    fn unsafe_redirect_destinations_do_not_become_join_links() {
        let text = "https://nam12.safelinks.protection.outlook.com/?url=javascript%3Aalert%281%29 https://www.google.com/url?q=file%3A%2F%2F%2Fetc%2Fhosts https://www.google.com/url?q=https%3A%2F%2Fuser%3Apassword%40meet.google.com%2Fabc-defg-hij https://www.google.com/url?q=https%3A%2F%2Fattacker.example%2Fjoin";

        assert_eq!(calendar_join_url(None, Some(text)), None);
    }

    #[test]
    fn deeply_nested_redirects_are_left_unrecognized() {
        let link = (0..32).fold(
            "https://meet.google.com/abc-defg-hij".to_string(),
            |link, _| {
                let mut wrapper =
                    url::Url::parse("https://nam12.safelinks.protection.outlook.com/").unwrap();
                wrapper.query_pairs_mut().append_pair("url", &link);
                wrapper.into()
            },
        );

        assert_eq!(calendar_join_url(None, Some(&link)), None);
    }

    #[test]
    fn a_meet_product_page_in_location_does_not_hide_the_call_in_notes() {
        let location = calendar_join_url(None, Some("https://meet.google.com/about/"));

        assert_eq!(
            calendar_join_url(location, Some("https://us02web.zoom.us/j/123456789")).as_deref(),
            Some("https://us02web.zoom.us/j/123456789")
        );
    }

    #[test]
    fn literal_terminal_query_punctuation_is_preserved() {
        let link =
            "https://www.google.com/url?q=https%3A%2F%2Fus02web.zoom.us%2Fj%2F123456789%3Fpwd%3DA;";

        assert_eq!(calendar_join_url(None, Some(link)).as_deref(), Some(link));
    }

    #[test]
    fn a_zoom_personal_room_link_in_calendar_details_is_a_join_link() {
        let link = "https://example.zoom.us/my/fixture-room?pwd=A%2Bb";

        assert_eq!(calendar_join_url(None, Some(link)).as_deref(), Some(link));
    }
}
