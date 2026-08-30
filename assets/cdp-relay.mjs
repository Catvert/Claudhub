// Diffuse l'écran du Chromium qui exécute une suite Pest, en lecture seule.
//
// Poussé dans le conteneur applicatif par `sail node -e`, il n'existe donc pas sur disque
// et n'ajoute rien au projet observé. Chromium n'ouvre son port CDP que sur son propre
// 127.0.0.1 et ignore `--remote-debugging-address` : c'est pourquoi le relais tourne là-bas
// et rend ses trames sur stdout, une ligne de JSON chacune.
//
// Le client PHP de Pest monopolise le websocket Playwright ; le CDP est un canal séparé,
// donc le test ne sait pas qu'il est observé. Le port lui est ouvert par le harnais du
// projet, qui lit `.pest-browser.json`.
//
// Le serveur Playwright est lancé ici aussi, et c'est ce qui rend le port possible : depuis
// playwright 1.62, `run-server` jette les `args` d'un client — `--remote-debugging-port`
// compris — à moins d'avoir été lancé avec `--unsafe` (filterLaunchOptions, `allowUnsafe`).
// Celui que le plugin Pest démarre pour son compte ne l'est pas et ne se paramètre pas,
// donc on en pose un à côté et le harnais s'y connecte le premier ; `Client::connectTo()`
// ne fait rien quand la connexion est déjà ouverte, si bien que celui du plugin, démarré
// quand même, ne sert à personne. Le nôtre meurt avec ce script.

const cdp = `127.0.0.1:${__PORT__}`
const serverPort = __SERVER_PORT__
const out = o => process.stdout.write(JSON.stringify(o) + '\n')
let attached = null

// Rien, depuis l'hôte, ne tue un processus lancé par `docker exec` : la fin de son stdin
// est le seul signal qui traverse. Claudhub tient ce tuyau ouvert le temps du run, et le
// referme à la fin — sans quoi le relais attendrait un navigateur jusqu'à la mort du
// conteneur, une fois par run. Le serveur Playwright part avec lui, pour la même raison :
// il n'a pas d'autre laisse.
let server = null

const stop = () => {
    server?.kill('SIGTERM')
    process.exit(0)
}

process.stdin.on('end', stop)
process.stdin.resume()
process.on('SIGTERM', stop)

if (serverPort > 0) {
    // `require` et non `import` : le script est passé à `node -e`, qui l'évalue en CommonJS
    // tant qu'il ne porte aucune syntaxe de module — un `await` de tête suffirait à le
    // faire refuser.
    const { spawn } = require('node:child_process')

    // Le binaire du projet, pas un `npx` qui irait le chercher sur le réseau : le conteneur
    // a déjà celui contre lequel la suite tourne, et deux versions de Playwright ne se
    // parlent pas — le serveur refuse un client trop vieux.
    server = spawn('node_modules/.bin/playwright', [
        'run-server', '--host', '127.0.0.1', '--port', String(serverPort), '--unsafe',
    ], { stdio: ['ignore', 'pipe', 'ignore'] })

    // « Listening on » est la ligne que le plugin attend lui aussi ; on la dit à Claudhub
    // pour le journal, et le harnais du projet, lui, attend le port par une boucle de
    // connexion — c'est lui qui doit tenir, la course étant de son côté.
    server.stdout.on('data', data => {
        if (String(data).includes('Listening on')) {
            out({ type: 'server', port: serverPort })
        }
    })

    server.on('error', () => out({ type: 'server-failed' }))
    server.on('exit', code => out({ type: 'server-gone', code }))
}

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
