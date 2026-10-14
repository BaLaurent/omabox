CFLAGS ?= -O2 -Wall -Wextra
PROTOS = wlr-screencopy-unstable-v1 xdg-output-unstable-v1 ext-foreign-toplevel-list-v1 ext-image-capture-source-v1 ext-image-copy-capture-v1
GEN = $(PROTOS:%=%-client-protocol.h) $(PROTOS:%=%-protocol.c)
omabox-burst: burst.c ../common/box.h ../common/roundtrip.h $(GEN)
	$(CC) $(CFLAGS) $(CPPFLAGS) $(LDFLAGS) -o $@ burst.c $(PROTOS:%=%-protocol.c) $$(pkg-config --cflags --libs wayland-client pixman-1 libpng) -lm
%-client-protocol.h: %.xml
	wayland-scanner client-header $< $@
%-protocol.c: %.xml
	wayland-scanner private-code $< $@
clean:
	rm -f omabox-burst $(GEN)
.PHONY: clean
