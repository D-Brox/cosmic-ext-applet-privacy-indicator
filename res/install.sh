#!/usr/bin/env bash

mkdir -p ~/.local/bin/ ${XDG_DATA_HOME:-~/.local/share}/{applications,metainfo}/
cp ./cosmic-ext-applet-privacy-indicator ~/.local/bin/
cp ./*.desktop ${XDG_DATA_HOME:-~/.local/share}/applications/
cp ./*.metainfo.xml ${XDG_DATA_HOME:-~/.local/share}/metainfo/
