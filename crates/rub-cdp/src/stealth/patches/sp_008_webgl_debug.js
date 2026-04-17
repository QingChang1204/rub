(() => {
    const webglVendor = __RUB_WEBGL_VENDOR__;
    const webglRenderer = __RUB_WEBGL_RENDERER__;
    const markNative = globalThis[Symbol.for('rub.stealth.mark_native')];
    const install = (Ctor) => {
        if (typeof Ctor === 'undefined' || !Ctor || Ctor.prototype.__rubWebglDebugInfo === true) return;
        const nativeGetParameter = Ctor.prototype.getParameter;
        const wrappedGetParameter = function getParameter(param) {
            if (param === 0x9245) return webglVendor;
            if (param === 0x9246) return webglRenderer;
            return nativeGetParameter.call(this, param);
        };
        Ctor.prototype.getParameter =
            typeof markNative === 'function'
                ? markNative(wrappedGetParameter, nativeGetParameter)
                : wrappedGetParameter;
        Object.defineProperty(Ctor.prototype, '__rubWebglDebugInfo', {
            value: true,
            configurable: true,
        });
    };
    install(globalThis.WebGLRenderingContext);
    install(globalThis.WebGL2RenderingContext);
})();
