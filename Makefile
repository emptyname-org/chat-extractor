.PHONY: build deb test lint audit source-audit validate-package run

build:
	cargo build --release
	mkdir -p dist
	install -m 0755 target/release/chatextractor dist/chatextractor

deb:
	./packaging/build-deb.sh

test:
	cargo test --all-targets
	GSETTINGS_BACKEND=memory GIO_USE_VFS=local GTK_A11Y=none LIBGL_ALWAYS_SOFTWARE=1 xvfb-run -a cargo test --lib app::tests::gtk_ui_uses_native_columns_and_export_dialogs -- --ignored --exact

lint:
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings

audit:
	cargo audit

source-audit:
	./scripts/audit-public-source.sh

validate-package: deb
	dpkg-deb --info $$(find dist -maxdepth 1 -type f -name 'chatextractor_*.deb' -printf '%p\n' | LC_ALL=C sort | tail -n 1)
	dpkg-deb --contents $$(find dist -maxdepth 1 -type f -name 'chatextractor_*.deb' -printf '%p\n' | LC_ALL=C sort | tail -n 1)
	desktop-file-validate packaging/org.chatextractor.app.desktop
	appstreamcli validate --no-net packaging/org.chatextractor.app.metainfo.xml

run:
	cargo run --release
