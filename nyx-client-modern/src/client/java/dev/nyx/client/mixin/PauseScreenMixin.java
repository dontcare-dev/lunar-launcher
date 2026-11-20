package dev.nyx.client.mixin;

import dev.nyx.client.screen.NyxSettingsScreen;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.components.Button;
import net.minecraft.client.gui.screens.PauseScreen;
import net.minecraft.network.chat.Component;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(PauseScreen.class)
public abstract class PauseScreenMixin {
    @Inject(method = "init", at = @At("TAIL"))
    private void nyx$addClientButton(CallbackInfo ci) {
        PauseScreen screen = (PauseScreen) (Object) this;
        int x = screen.width - 118;
        ((ScreenAccessor) screen).nyx$addNyxButton(Button.builder(Component.literal("NYX CLIENT"), button ->
            Minecraft.getInstance().setScreen(new NyxSettingsScreen(screen))
        ).bounds(x, 10, 108, 22).build());
    }
}
