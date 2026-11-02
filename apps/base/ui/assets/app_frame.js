// Generic bridge between a same-origin MCP App document and one mounted live
// page. It grants no operation: every tools/call is an owner-decoded event.
(() => {
  'use strict';
  const AppFrame = {
    mounted() {
      this.frame = this.el.querySelector('iframe');
      this.send = message => this.frame?.contentWindow?.postMessage(message, location.origin);
      this.message = event => {
        if (event.origin !== location.origin || event.source !== this.frame?.contentWindow) return;
        const message = event.data;
        if (!message || message.jsonrpc !== '2.0') return;
        if (message.method === 'ui/initialize') {
          this.send({jsonrpc:'2.0', id:message.id, result:{protocolVersion:'2026-01-26', hostInfo:{name:'bilibi-browser',version:'1'}, hostCapabilities:{}, hostContext:{theme:document.documentElement.dataset.theme || 'light',displayMode:'inline',availableDisplayModes:['inline'],platform:'web'}}});
          return;
        }
        if (message.method === 'ui/notifications/initialized') {
          this.emit('app-frame', {bridge: JSON.stringify({method:'open'})});
          return;
        }
        if (message.method === 'ui/notifications/size-changed') return;
        if (message.method === 'ui/update-model-context') {
          this.send({jsonrpc:'2.0', id:message.id, result:{}});
          return;
        }
        if (message.method === 'ui/open-link') {
          try {
            const url = new URL(message.params?.url, location.href);
            if (url.origin !== location.origin) throw new Error('refused');
            window.open(url.href, '_blank', 'noopener');
            this.send({jsonrpc:'2.0', id:message.id, result:{}});
          } catch (_) {
            this.send({jsonrpc:'2.0', id:message.id, error:{code:-32602,message:'Only links in this Bilibi may be opened.'}});
          }
          return;
        }
        if (message.method === 'ui/request-display-mode') {
          this.send({jsonrpc:'2.0', id:message.id, result:{mode:'inline'}});
          return;
        }
        if (message.method === 'ui/message') {
          this.send({jsonrpc:'2.0', id:message.id, error:{code:-32601,message:'Continue in the AI app.'}});
          return;
        }
        if (message.method === 'tools/call') {
          this.emit('app-frame', {bridge: JSON.stringify(message)});
        }
      };
      this.reply = event => {
        const detail = event.detail;
        if (detail?.frame !== this.el.id) return;
        if (detail.message) this.send(detail.message);
      };
      window.addEventListener('message', this.message);
      window.addEventListener('bilibi:app-frame', this.reply);
    },
    destroyed() {
      window.removeEventListener('message', this.message);
      window.removeEventListener('bilibi:app-frame', this.reply);
    },
  };
  window.BilibiUI.registerHook('AppFrame', AppFrame);
})();
