(() => {
    if (!window.chrome) window.chrome = {};
    if (!window.chrome.app) {
        window.chrome.app = {
            isInstalled: false,
            InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' },
            RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' },
            getDetails: function() { return null; },
            getIsInstalled: function() { return false; },
        };
    }
    if (!window.chrome.csi) {
        window.chrome.csi = function() { return {}; };
    }
    if (!window.chrome.loadTimes) {
        window.chrome.loadTimes = function() { return {}; };
    }
})();
