build:
	@cargo build

test:
	@cargo nextest run --all-features

build-release:
	@echo "Building coda-server in release mode..."
	cargo build --release --bin coda-server

deploy: build-release install-service
	@echo "Restarting coda-server..."
	sudo systemctl restart coda-server
	@echo "Deploy complete."
	@systemctl status coda-server --no-pager

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

restart-service:
	sudo systemctl restart coda-server

status-service:
	@systemctl status coda-server --no-pager

install-sudoers:
	@echo "Installing passwordless sudo rules for coda-server deployment..."
	sudo cp deploy/sudoers-coda-deploy /etc/sudoers.d/coda-deploy
	sudo chmod 0440 /etc/sudoers.d/coda-deploy
	sudo visudo -cf /etc/sudoers.d/coda-deploy
	@echo "Done. Sudo password no longer required for deploy commands."

.PHONY: build test build-release deploy install-service uninstall-service restart-service status-service install-sudoers
