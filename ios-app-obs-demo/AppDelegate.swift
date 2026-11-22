import UIKit

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    private let status = UILabel()
    private var buttons: [UIButton] = []
    func application(_ application: UIApplication, didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        let controller = UIViewController()
        controller.view.backgroundColor = .systemBackground
        status.text = "Ready: synthetic fixture only"
        let stack = UIStackView(arrangedSubviews: [status])
        stack.axis = .vertical; stack.spacing = 16
        for variant in 0...1 {
            let button = UIButton(type: .system)
            button.setTitle(variant == 0 ? "Run fixture A" : "Run fixture B", for: .normal)
            button.tag = variant
            button.addTarget(self, action: #selector(run(_:)), for: .touchUpInside)
            buttons.append(button); stack.addArrangedSubview(button)
        }
        stack.translatesAutoresizingMaskIntoConstraints = false
        controller.view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.centerXAnchor.constraint(equalTo: controller.view.centerXAnchor),
            stack.centerYAnchor.constraint(equalTo: controller.view.centerYAnchor)
        ])
        window.rootViewController = controller; window.makeKeyAndVisible(); self.window = window
        return true
    }
    @objc private func run(_ sender: UIButton) {
        let accepted = MIDemoSubmit(UInt32(sender.tag)) { [weak self] result in
            self?.status.text = result == 0 ? "Passed and closed" : "Failed"
            self?.buttons.forEach { $0.isEnabled = result == 0 }
        }
        if accepted {
            status.text = "Running"
            buttons.forEach { $0.isEnabled = false }
        } else { status.text = "Busy" }
    }
}
