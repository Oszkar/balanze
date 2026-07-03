pub(crate) mod jsonl;
pub(crate) mod oauth_poll;
pub(crate) mod openai_poll;
pub(crate) mod safety;
pub(crate) mod statusline;

pub(crate) fn get_or_build_client(
    client: &mut Option<reqwest::Client>,
) -> Result<&reqwest::Client, reqwest::Error> {
    match client {
        Some(existing) => Ok(existing),
        slot @ None => {
            let built = reqwest::Client::builder()
                .user_agent("balanze-watcher/0.1.0")
                .timeout(std::time::Duration::from_secs(30))
                .build()?;
            Ok(slot.insert(built))
        }
    }
}
