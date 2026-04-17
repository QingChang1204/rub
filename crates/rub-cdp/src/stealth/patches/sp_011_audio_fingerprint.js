(() => {
    const firstIndex = __RUB_AUDIO_FIRST_INDEX__;
    const secondIndex = __RUB_AUDIO_SECOND_INDEX__;
    const delta = __RUB_AUDIO_DELTA__;
    const markNative = globalThis[Symbol.for('rub.stealth.mark_native')];
    const touchedChannels = new WeakMap();
    const install = () => {
        if (typeof AudioBuffer === 'undefined' || !AudioBuffer.prototype || AudioBuffer.prototype.__rubAudioFingerprint === true) return;
        const nativeGetChannelData = AudioBuffer.prototype.getChannelData;
        const wrappedGetChannelData = function getChannelData(channel) {
            const data = nativeGetChannelData.call(this, channel);
            try {
                let seen = touchedChannels.get(this);
                if (!seen) {
                    seen = new Set();
                    touchedChannels.set(this, seen);
                }
                const key = `${channel}:${data.length}`;
                if (!seen.has(key)) {
                    if (firstIndex < data.length) data[firstIndex] += delta;
                    if (secondIndex < data.length) data[secondIndex] += delta;
                    seen.add(key);
                }
            } catch (_) {}
            return data;
        };
        AudioBuffer.prototype.getChannelData =
            typeof markNative === 'function'
                ? markNative(wrappedGetChannelData, nativeGetChannelData)
                : wrappedGetChannelData;
        if (typeof AudioBuffer.prototype.copyFromChannel === 'function') {
            const nativeCopyFromChannel = AudioBuffer.prototype.copyFromChannel;
            const wrappedCopyFromChannel = function copyFromChannel(destination, channel, startInChannel) {
                try {
                    this.getChannelData(channel);
                } catch (_) {}
                return nativeCopyFromChannel.call(this, destination, channel, startInChannel);
            };
            AudioBuffer.prototype.copyFromChannel =
                typeof markNative === 'function'
                    ? markNative(wrappedCopyFromChannel, nativeCopyFromChannel)
                    : wrappedCopyFromChannel;
        }
        Object.defineProperty(AudioBuffer.prototype, '__rubAudioFingerprint', {
            value: true,
            configurable: true,
        });
    };
    install();
})();
