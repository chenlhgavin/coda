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

install-service:
	@echo "Installing coda-server systemd service..."
	sudo cp deploy/coda-server.service /etc/systemd/system/
	sudo systemctl daemon-reload
	@echo "Done. Run 'sudo systemctl enable --now coda-server' to start."

uninstall-service:
	@echo "Removing coda-server systemd service..."
	-sudo systemctl disable --now coda-server
	sudo rm -f /etc/systemd/system/coda-server.service
	sudo systemctl daemon-reload
	@echo "Done."

.PHONY: build test release update-submodule publish-dry-run publish install-service uninstall-service
