# Security

Chat Extractor for Signal processes exports locally without a network connection.

Report security issues through GitHub security advisories. Use only synthetic reproductions; never attach real Signal exports, chats, media, local paths, credentials, or other private data to public issues or pull requests.

Before publishing source or releases, run:

```sh
make source-audit
make test
make lint
make audit
```
