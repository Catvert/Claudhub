// Diffuse l'écran du Chromium qui exécute une suite Pest, en lecture seule.
//
// Poussé dans le conteneur applicatif par `sail node -e`, il n'existe donc pas sur disque
// et n'ajoute rien au projet observé. Chromium n'ouvre son port CDP que sur son propre
// 127.0.0.1 et ignore `--remote-debugging-address` : c'est pourquoi le relais tourne là-bas
// et rend ses trames sur stdout, une ligne de JSON chacune.
//
// Le client PHP de Pest monopolise le websocket Playwright ; le CDP est un canal séparé,
// donc le test ne sait pas qu'il est observé. Le port lui est ouvert par le harnais du
// projet, qui lit `.pest-cdp-port`.

const cdp = `127.0.0.1:${__PORT__}`
const out = o => process.stdout.write(JSON.stringify(o) + '\n')
let attached = null

;(async function connect() {
    let endpoint

    try {
        const response = await fetch(`http://${cdp}/json/version`, { signal: AbortSignal.timeout(1500) })
        endpoint = (await response.json()).webSocketDebuggerUrl
    } catch {
        return setTimeout(connect, 1000)
    }

    const ws = new WebSocket(endpoint)
    const pending = new Map()
    let nextId = 1

    const send = (method, params = {}, sessionId) => new Promise(resolve => {
        const id = nextId++
        pending.set(id, resolve)
        ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }))
    })

    ws.onopen = () => send('Target.setDiscoverTargets', { discover: true })
    ws.onerror = () => {}

    // Le navigateur meurt avec le run ; on se remet en attente du suivant plutôt que
    // de sortir, parce qu'une campagne enchaîne plusieurs commandes.
    ws.onclose = () => {
        attached = null
        out({ type: 'detached' })
        setTimeout(connect, 500)
    }

    ws.onmessage = async ({ data }) => {
        const message = JSON.parse(data)

        if (message.id !== undefined) {
            pending.get(message.id)?.(message.result)
            pending.delete(message.id)

            return
        }

        // Chaque `visit()` crée un contexte et une page : la cible change à chaque test,
        // et la plus récente est toujours celle que le test pilote.
        if (message.method === 'Target.targetCreated' && message.params.targetInfo.type === 'page') {
            const { sessionId } = await send('Target.attachToTarget', {
                targetId: message.params.targetInfo.targetId,
                flatten: true,
            })

            attached = sessionId

            await send('Page.enable', {}, sessionId)
            await send('Page.startScreencast', {
                format: 'jpeg',
                quality: __QUALITY__,
                maxWidth: __WIDTH__,
                maxHeight: __HEIGHT__,
                everyNthFrame: __EVERY__,
            }, sessionId)

            return out({ type: 'attached' })
        }

        if (message.method === 'Page.screencastFrame' && message.sessionId === attached) {
            // L'acquittement est ce qui déclenche la trame suivante : sans lui le flux
            // s'arrête après quelques images.
            send('Page.screencastFrameAck', { sessionId: message.params.sessionId }, message.sessionId)

            out({
                type: 'frame',
                data: message.params.data,
                width: message.params.metadata.deviceWidth,
                height: message.params.metadata.deviceHeight,
            })
        }
    }
})()
