(() => {
    const redOffset = __RUB_CANVAS_RED_OFFSET__;
    const greenOffset = __RUB_CANVAS_GREEN_OFFSET__;
    const blueOffset = __RUB_CANVAS_BLUE_OFFSET__;
    const markNative = globalThis[Symbol.for('rub.stealth.mark_native')];
    const clamp = (value) => Math.max(0, Math.min(255, value));
    const applyNoise = (data) => {
        if (!data || typeof data.length !== 'number' || data.length < 4) return data;
        data[0] = clamp(data[0] + redOffset);
        data[1] = clamp(data[1] + greenOffset);
        data[2] = clamp(data[2] + blueOffset);
        return data;
    };
    const makeCanvasClone = (source) => {
        const width = Number(source && source.width) || 0;
        const height = Number(source && source.height) || 0;
        if (typeof document !== 'undefined' && typeof document.createElement === 'function') {
            const clone = document.createElement('canvas');
            clone.width = width;
            clone.height = height;
            return clone;
        }
        if (typeof OffscreenCanvas !== 'undefined') {
            return new OffscreenCanvas(width, height);
        }
        return null;
    };
    const installContextPatch = (Ctor) => {
        if (typeof Ctor === 'undefined' || !Ctor || Ctor.prototype.__rubCanvasFingerprint === true) return;
        const nativeGetImageData = Ctor.prototype.getImageData;
        const wrappedGetImageData = function getImageData(...args) {
            const result = nativeGetImageData.apply(this, args);
            try {
                applyNoise(result && result.data);
            } catch (_) {}
            return result;
        };
        Ctor.prototype.getImageData =
            typeof markNative === 'function'
                ? markNative(wrappedGetImageData, nativeGetImageData)
                : wrappedGetImageData;
        Object.defineProperty(Ctor.prototype.getImageData, '__rubNativeGetImageData', {
            value: nativeGetImageData,
            configurable: true,
        });
        Object.defineProperty(Ctor.prototype, '__rubCanvasFingerprint', {
            value: true,
            configurable: true,
        });
    };
    const readNativeImageData = (ctx, width, height) => {
        if (!ctx || typeof ctx.getImageData !== 'function') return null;
        const nativeGetImageData = ctx.getImageData.__rubNativeGetImageData;
        if (typeof nativeGetImageData === 'function') {
            return nativeGetImageData.call(ctx, 0, 0, width, height);
        }
        return ctx.getImageData(0, 0, width, height);
    };
    const cloneWithNoise = (source) => {
        const clone = makeCanvasClone(source);
        if (!clone || typeof clone.getContext !== 'function') return null;
        const ctx = clone.getContext('2d');
        if (!ctx) return null;
        ctx.drawImage(source, 0, 0);
        const width = Number(clone.width) || 0;
        const height = Number(clone.height) || 0;
        if (width > 0 && height > 0) {
            const imageData = readNativeImageData(ctx, width, height);
            if (imageData) {
                applyNoise(imageData.data);
                ctx.putImageData(imageData, 0, 0);
            }
        }
        return clone;
    };
    const installCanvasMethodPatch = (Ctor, method, marker) => {
        if (typeof Ctor === 'undefined' || !Ctor || typeof Ctor.prototype[method] !== 'function') return;
        if (Ctor.prototype[marker] === true) return;
        const nativeMethod = Ctor.prototype[method];
        const wrappedMethod = function(...args) {
            try {
                const clone = cloneWithNoise(this);
                if (clone && typeof nativeMethod === 'function') {
                    return nativeMethod.apply(clone, args);
                }
            } catch (_) {}
            return nativeMethod.apply(this, args);
        };
        Ctor.prototype[method] =
            typeof markNative === 'function'
                ? markNative(wrappedMethod, nativeMethod)
                : wrappedMethod;
        Object.defineProperty(Ctor.prototype, marker, {
            value: true,
            configurable: true,
        });
    };

    installContextPatch(globalThis.CanvasRenderingContext2D);
    installContextPatch(globalThis.OffscreenCanvasRenderingContext2D);
    installCanvasMethodPatch(globalThis.HTMLCanvasElement, 'toDataURL', '__rubCanvasToDataURL');
    installCanvasMethodPatch(globalThis.HTMLCanvasElement, 'toBlob', '__rubCanvasToBlob');
    installCanvasMethodPatch(globalThis.OffscreenCanvas, 'convertToBlob', '__rubCanvasConvertToBlob');
})();
