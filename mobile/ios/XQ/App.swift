import UIKit

// XQ（egui）iOS 外殼：UIScene 生命週期（Xcode 27／iOS 27 SDK 起必須；winit／eframe 還不支援，所以不用它們），
// 畫面是一個 CAMetalLayer，Rust 那邊（ioshost.rs）用 egui-wgpu 直接畫上去。
// CADisplayLink 每幀問 Rust「要不要畫」，只看有沒有東西要畫，不看輸入焦點：
// 控制中心、通知、來電橫幅蓋住時照畫（Android 版的凍結問題不會在這裡發生）；只有進背景才停。

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     configurationForConnecting connectingSceneSession: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let config = UISceneConfiguration(name: "Default", sessionRole: connectingSceneSession.role)
        config.delegateClass = SceneDelegate.self
        return config
    }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let windowScene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: windowScene)
        window.rootViewController = XQViewController()
        window.makeKeyAndVisible()
        self.window = window
    }

    private var vc: XQViewController? { window?.rootViewController as? XQViewController }
    func sceneWillEnterForeground(_ scene: UIScene) { vc?.startFrames() }
    func sceneDidEnterBackground(_ scene: UIScene) { vc?.stopFrames() }
}

/// 畫布：CAMetalLayer＋觸控＋鍵盤輸入（UIKeyInput）
final class EguiView: UIView, UIKeyInput {
    var host: OpaquePointer?
    override class var layerClass: AnyClass { CAMetalLayer.self }

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        (layer as? CAMetalLayer)?.isOpaque = true
    }
    required init?(coder: NSCoder) { fatalError() }

    private func send(_ touches: Set<UITouch>, _ phase: UInt32) {
        guard let h = host else { return }
        for t in touches {
            let p = t.location(in: self)
            let id = UInt64(UInt(bitPattern: ObjectIdentifier(t).hashValue))
            let force = t.maximumPossibleForce > 0 ? Float(t.force / t.maximumPossibleForce) : 0
            xq_egui_touch(h, id, phase, Float(p.x), Float(p.y), force)
        }
    }
    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, 0) }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, 1) }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, 2) }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, 3) }

    // 鍵盤：搜尋框有焦點時 Rust 回報「要鍵盤」，這裡就 becomeFirstResponder 叫出系統鍵盤
    override var canBecomeFirstResponder: Bool { true }
    var hasText: Bool { true }
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var returnKeyType: UIReturnKeyType = .search
    func insertText(_ text: String) {
        guard let h = host else { return }
        text.withCString { xq_egui_text(h, $0) }
    }
    func deleteBackward() {
        guard let h = host else { return }
        xq_egui_backspace(h)
    }
}

final class XQViewController: UIViewController {
    private let canvas = EguiView(frame: .zero)
    private var displayLink: CADisplayLink?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = UIColor(red: 0.08, green: 0.09, blue: 0.11, alpha: 1)
        view.addSubview(canvas)
    }

    private var scale: CGFloat { view.window?.windowScene?.screen.scale ?? traitCollection.displayScale }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        // 只用安全區域（避開瀏海／動態島／底部橫條）
        let frame = view.bounds.inset(by: view.safeAreaInsets)
        guard frame.width > 0, frame.height > 0 else { return }
        let s = scale
        canvas.frame = frame
        canvas.contentScaleFactor = s
        canvas.layer.contentsScale = s
        let w = UInt32(frame.width * s), h = UInt32(frame.height * s)
        if let host = canvas.host {
            xq_egui_resize(host, w, h, Float(s))
        } else {
            canvas.host = xq_egui_create(Unmanaged.passUnretained(canvas.layer).toOpaque(), w, h, Float(s))
            if canvas.host == nil { NSLog("[xq] egui 初始化失敗（看上面的 Rust log）") }
            startFrames()
        }
    }

    func startFrames() {
        guard displayLink == nil, canvas.host != nil else { return }
        let link = CADisplayLink(target: self, selector: #selector(tick))
        link.add(to: .main, forMode: .common)
        displayLink = link
        if let h = canvas.host { xq_egui_wake(h) }
    }

    func stopFrames() {
        displayLink?.invalidate()
        displayLink = nil
    }

    @objc private func tick() {
        guard let h = canvas.host else { return }
        let flags = xq_egui_frame(h)
        let wantKeyboard = flags & 2 != 0
        if wantKeyboard && !canvas.isFirstResponder { canvas.becomeFirstResponder() }
        if !wantKeyboard && canvas.isFirstResponder { canvas.resignFirstResponder() }
    }

    override var preferredStatusBarStyle: UIStatusBarStyle { .lightContent }
}
