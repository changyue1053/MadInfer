import UIKit
import MadInfer

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        let controller = UIViewController()
        controller.view.backgroundColor = .systemBackground
        let label = UILabel()
        label.text = "MadInfer binary consumer smoke tests"
        label.textAlignment = .center
        label.translatesAutoresizingMaskIntoConstraints = false
        controller.view.addSubview(label)
        NSLayoutConstraint.activate([
            label.centerXAnchor.constraint(equalTo: controller.view.centerXAnchor),
            label.centerYAnchor.constraint(equalTo: controller.view.centerYAnchor)
        ])
        window.rootViewController = controller
        window.makeKeyAndVisible()
        self.window = window
        return true
    }
}
