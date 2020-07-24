#! /usr/bin/env bash
set -e -o verbose

brew install ruby

export PATH="/usr/local/opt/ruby/bin:$PATH"' >> "$HOME/.bash_profile"
