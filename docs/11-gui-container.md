// For format details, see https://aka.ms/devcontainer.json. For config options, see the
// README at: https://github.com/devcontainers/templates/tree/main/src/javascript-node
{
	"name": "gui_agent_dev",
	// Build the image from the local Dockerfile (base image: gui_agent_dev:7.10.1).
	// More info: https://containers.dev/guide/dockerfile
	"image": "gui_agent_dev:7.10.2",
	// "features": {},
	// Pass the host NVIDIA GPU (RTX 5070 Ti, Blackwell) into the container.
	// Needs nvidia-container-toolkit on the host (and driver >=570 for the 5070 Ti /
	// CUDA 12.8). "optional" keeps the container bootable without a GPU. Honored on
	// "Rebuild Container". If your runtime ignores hostRequirements, instead add
	// "--gpus","all" to runArgs below.
	"mounts": [
		"type=bind,source=${localEnv:HOME},target=/home/host",
		"type=bind,source=/tmp/.X11-unix,target=/tmp/.X11-unix",
		"type=bind,source=/tmp/runtime-dir,target=/tmp/runtime-dir",
		"type=bind,source=/run/,target=/run/",
		// "type=bind,source=/home/jiugui5209/Public,target=/home/tmp/",
	],
	"runArgs": [
		"--security-opt",
		"apparmor=unconfined",
		"--pid=host",
		"--gpus=all",
		"--net=host",
		"--device=/dev/uinput:/dev/uinput"
	],
	"containerEnv": {
		"DISPLAY": ":0",
		"XDG_RUNTIME_DIR": "/run/user/1000",
		"WAYLAND_DISPLAY": "wayland-0",
		// 告诉 NVIDIA 运行时在容器内暴露哪些能力，all 代表暴露全部能力（包括 CUDA, NVENC 编解码等）
		"NVIDIA_DRIVER_CAPABILITIES": "all",
		"NVIDIA_VISIBLE_DEVICES": "all"
	},
	// Features to add to the dev container. More info: https://containers.dev/features.
	// "features": {},
	// Use 'forwardPorts' to make a list of ports inside the container available locally.
	"forwardPorts": [],
	// Use 'postCreateCommand' to run commands after the container is created.
	// Configure tool-specific properties.
	// "customizations": {},
	// Uncomment to connect as root instead. More info: https://aka.ms/dev-containers-non-root.
	"remoteUser": "node"
}

