build:
	@cargo build

test:
	@cargo nextest run --all-features

release:
	@cargo release tag --execute
	@git cliff -o CHANGELOG.md
	@git commit -a -n -m "Update CHANGELOG.md" || true
	@git push origin master
	@cargo release push --execute

update-submodule:
	@git submodule update --init --recursive --remote

publish-dry-run:
	cargo publish --dry-run --allow-dirty -p coda-pm
	cargo publish --dry-run --allow-dirty -p coda-core
	cargo publish --dry-run --allow-dirty -p coda-cli

publish:
	cargo publish -p coda-pm
	sleep 30
	cargo publish -p coda-core
	sleep 30
	cargo publish -p coda-cli

.PHONY: build test release update-submodule publish-dry-run publish
