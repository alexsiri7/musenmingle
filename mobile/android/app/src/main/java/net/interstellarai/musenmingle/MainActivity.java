package net.interstellarai.musenmingle;

import android.os.Bundle;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebView;
import com.getcapacitor.Bridge;
import com.getcapacitor.BridgeActivity;
import com.getcapacitor.BridgeWebViewClient;

public class MainActivity extends BridgeActivity {

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // Null when the phone has no WebView; BridgeActivity then shows its own message.
        if (bridge != null) {
            bridge.setWebViewClient(new SiteErrorPagesClient(bridge));
        }
    }

    /**
     * Capacitor shows `server.errorPath` (the offline page) for HTTP errors
     * too; a 404 or a form's 400 is the site's own page, so only real
     * connection failures (onReceivedError) get the offline page.
     */
    private static class SiteErrorPagesClient extends BridgeWebViewClient {

        SiteErrorPagesClient(Bridge bridge) {
            super(bridge);
        }

        @Override
        public void onReceivedHttpError(WebView view, WebResourceRequest request, WebResourceResponse errorResponse) {}
    }
}
