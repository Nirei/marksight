PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
APP_ID ?= io.github.marksight.Marksight

.PHONY: all install uninstall clean check deb

all:
	cargo build --release

check:
	cargo test --lib --test snapshots --test property --test cli
	cargo clippy --all-targets -- -D warnings

install:
	@test -x target/release/marksight || { echo "no release binary — run 'make' as your user first" >&2; exit 1; }
	install -Dm755 target/release/marksight -t $(DESTDIR)$(BINDIR)
	install -Dm644 data/$(APP_ID).desktop -t $(DESTDIR)$(DATADIR)/applications
	install -Dm644 data/$(APP_ID).metainfo.xml -t $(DESTDIR)$(DATADIR)/metainfo
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg \
		$(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/marksight.1 -t $(DESTDIR)$(DATADIR)/man/man1

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/marksight
	rm -f $(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop
	rm -f $(DESTDIR)$(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	rm -f $(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	rm -f $(DESTDIR)$(DATADIR)/man/man1/marksight.1

deb:
	packaging/deb/build-local.sh

clean:
	cargo clean
