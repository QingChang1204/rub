(() => {
    if (!window.chrome) window.chrome = {};
    if (!window.chrome.runtime) {
        window.chrome.runtime = {
            id: undefined,
            connect: function() { return {}; },
            sendMessage: function() {},
            onMessage: { addListener: function() {}, removeListener: function() {} },
            onConnect: { addListener: function() {}, removeListener: function() {} },
        };
    }
})();
