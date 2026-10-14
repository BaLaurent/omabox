CFLAGS ?= -O2 -Wall -Wextra
omabox-tree: tree.c ../common/box.h
	$(CC) $(CFLAGS) $(CPPFLAGS) $(LDFLAGS) -o $@ tree.c $$(pkg-config --cflags --libs atspi-2 gio-2.0)
clean:
	rm -f omabox-tree
.PHONY: clean
