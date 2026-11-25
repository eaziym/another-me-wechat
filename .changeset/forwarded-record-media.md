---
"@agent-wechat/agent-wechat": minor
"@agent-wechat/shared": minor
"@agent-wechat/rust-crates": minor
---

Pictures, videos and files inside forwarded chat records, including nested records, can now be fetched. The record's text shows each one as `[image item=N]`, `[video item=N]` or `[file item=N] name`, and `GET /v1/accounts/{account}/messages/{id}/media` lists one asset per item with a new `recordItem` field holding that number. Download them with the usual `media-transfers` request; they use the same CDN transfer and size/MD5 checks as photo, video and file messages, stay in the daemon's own cache rather than the official profile, and are never downloaded automatically. Record thumbnails are not listed.
