# Assemble the FSKit extension into a codesigned .appex inside a host .app.
#
# TEMPLATE / best-effort. FSKit extensions are normally built by an Xcode "File
# System Extension" target; this Makefile documents the equivalent hand-assembly
# for when the Rust cdylib is the extension binary. It requires:
#   * full Xcode (for a valid FSKit target template to crib the plists from) and
#   * a codesigning identity (SIGN_ID), else fskitd will refuse to load it.
#
# Once built + signed, the module appears under:
#   System Settings ▸ General ▸ Login Items & Extensions ▸ File System Extensions
# and can be mounted with `mount -F -t fskit-s3 <resource> <mountpoint>`.

SIGN_ID   ?= -                       # "-" = ad-hoc; set to a Developer ID to distribute
APP        = build/fskit-s3.app
EXT        = $(APP)/Contents/PlugIns/fskit-s3.appex
EXT_BIN    = target/release/libfskit_s3_ext.dylib

.PHONY: all bundle sign clean
all: bundle sign

$(EXT_BIN):
	cargo build --release -p fskit-s3-ext

bundle: $(EXT_BIN)
	mkdir -p $(EXT)/Contents/MacOS $(APP)/Contents/MacOS
	cp $(EXT_BIN) $(EXT)/Contents/MacOS/fskit_s3_ext
	cp bundle/Extension-Info.plist $(EXT)/Contents/Info.plist
	cp bundle/App-Info.plist $(APP)/Contents/Info.plist
	@echo "Assembled $(APP)"

sign: bundle
	codesign --force --sign "$(SIGN_ID)" \
		--entitlements bundle/Extension.entitlements --options runtime $(EXT)
	codesign --force --sign "$(SIGN_ID)" --options runtime $(APP)
	@echo "Signed with identity: $(SIGN_ID)"

clean:
	rm -rf build
